#!/usr/bin/env bats

@test "legacy scripts print retirement notice and exit 2" {
  run bash "$BATS_TEST_DIRNAME/../scripts/send.sh" core alice bob hello

  [ "$status" -eq 2 ]
  [[ "$output" == *"magi legacy script send.sh is retired."* ]]
  [[ "$output" == *"Use ~/.local/bin/magi or ~/.agents/skills/magi/bin/magi instead."* ]]
}

@test "legacy scripts do not create sqlite state" {
  temp_dir="$(mktemp -d)"
  trap 'rm -rf "$temp_dir"' EXIT

  run bash "$BATS_TEST_DIRNAME/../scripts/init-db.sh"

  [ "$status" -eq 2 ]
  [ ! -e "$temp_dir/messages.db" ]
}
