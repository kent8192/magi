-- Atomically validate and consume an invite token.
--
-- KEYS[1] = invite-token lookup key (magi:invite_token:<hash>)
-- ARGV[1] = expected SHA-256 token hash
-- ARGV[2] = current unix time in seconds
--
-- Returns one of:
--   {"invalid"}                         token unknown / hash mismatch / corrupt
--   {"revoked"}                         invite was revoked
--   {"expired"}                         invite past its expiry
--   {"max_uses"}                        invite already at its use limit
--   {"ok", <team>, <invite_key>}        consumed; used_count incremented
--
-- All checks and the used_count increment run in a single server-side script
-- so concurrent joins cannot race past revocation, expiry, or max_uses. Redis
-- executes the whole EVAL/EVALSHA body atomically (no other command can
-- interleave), which is exactly why this validate-then-consume sequence is
-- implemented as a Lua script instead of separate round trips.

-- Resolve the token-lookup key to the canonical invite hash key. GET returns
-- false (Lua nil) when no such mapping exists.
local invite_key = redis.call("GET", KEYS[1])
-- Early return: the token hash is unknown, so the token is invalid.
if not invite_key then
  return {"invalid"}
end

-- Guard against a dangling lookup key: the mapping exists but the invite hash
-- it points to is gone. Clean up the stale pointer (DEL) and report invalid.
if redis.call("EXISTS", invite_key) == 0 then
  redis.call("DEL", KEYS[1])
  return {"invalid"}
end

-- Defense-in-depth: re-verify the stored token hash matches the caller's hash.
-- Protects against key collisions / tampering even after the GET lookup.
local stored_hash = redis.call("HGET", invite_key, "token_hash")
-- Early return: stored hash absent (nil) or not equal to the expected hash.
if stored_hash ~= ARGV[1] then
  return {"invalid"}
end

-- Revocation check: a non-empty "revoked_at" field marks the invite revoked.
local revoked_at = redis.call("HGET", invite_key, "revoked_at")
-- Early return: invite was explicitly revoked.
if revoked_at and revoked_at ~= "" then
  return {"revoked"}
end

-- Read the expiry timestamp; tonumber yields nil if the field is missing or
-- non-numeric (treated as corrupt below).
local expires_at = tonumber(redis.call("HGET", invite_key, "expires_at"))
-- Early return: missing/corrupt expiry field is treated as invalid.
if not expires_at then
  return {"invalid"}
end

-- Expiry check against the caller-supplied current time (ARGV[2]). On expiry,
-- delete the lookup key so the dead token stops resolving, then report expired.
if expires_at <= tonumber(ARGV[2]) then
  redis.call("DEL", KEYS[1])
  return {"expired"}
end

-- Read the raw use-counter fields, defaulting to "0" when absent so a fresh
-- invite reads as 0 uses with an unlimited (0) cap.
local used_count_raw = redis.call("HGET", invite_key, "used_count") or "0"
local max_uses_raw = redis.call("HGET", invite_key, "max_uses") or "0"
-- Counters must be non-negative decimal integers (used_count is only ever
-- written via HINCRBY). Reject any other shape as corrupt and fail closed,
-- matching the documented {"invalid"} contract above. A bare tonumber()
-- guard is insufficient: it would accept "1.5", "-5", "0x10", "1e3", and
-- leading/trailing whitespace, so corrupt counters could slip past.
if not string.match(used_count_raw, "^%d+$") or not string.match(max_uses_raw, "^%d+$") then
  return {"invalid"}
end
-- Both fields are validated integer strings; convert them for numeric comparison.
local used_count = tonumber(used_count_raw)
local max_uses = tonumber(max_uses_raw)

-- max_uses == 0 means "unlimited"; only enforce the cap when it is positive.
-- Early return: the invite has already reached its use limit.
if max_uses > 0 and used_count >= max_uses then
  return {"max_uses"}
end

-- Consume the invite: atomically bump used_count. Because the whole script is
-- atomic, this increment is safe against concurrent consumers.
redis.call("HINCRBY", invite_key, "used_count", 1)
-- Success: return the team name (for membership) and the invite key alongside
-- the "ok" marker.
return {"ok", redis.call("HGET", invite_key, "team"), invite_key}
