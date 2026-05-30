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
-- so concurrent joins cannot race past revocation, expiry, or max_uses.
local invite_key = redis.call("GET", KEYS[1])
if not invite_key then
  return {"invalid"}
end

if redis.call("EXISTS", invite_key) == 0 then
  redis.call("DEL", KEYS[1])
  return {"invalid"}
end

local stored_hash = redis.call("HGET", invite_key, "token_hash")
if stored_hash ~= ARGV[1] then
  return {"invalid"}
end

local revoked_at = redis.call("HGET", invite_key, "revoked_at")
if revoked_at and revoked_at ~= "" then
  return {"revoked"}
end

local expires_at = tonumber(redis.call("HGET", invite_key, "expires_at"))
if not expires_at then
  return {"invalid"}
end

if expires_at <= tonumber(ARGV[2]) then
  redis.call("DEL", KEYS[1])
  return {"expired"}
end

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
local used_count = tonumber(used_count_raw)
local max_uses = tonumber(max_uses_raw)
if max_uses > 0 and used_count >= max_uses then
  return {"max_uses"}
end

redis.call("HINCRBY", invite_key, "used_count", 1)
return {"ok", redis.call("HGET", invite_key, "team"), invite_key}
