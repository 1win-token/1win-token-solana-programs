---
paths:
  - "**/*.rs"
---

# Validating an instruction's arguments at the entry

## The core

An instruction's arguments (numbers, strings, vectors) come from an untrusted party and are checked at the entry - before anything is written to state and before any CPI, not "somewhere later at the point of use". Strings and vectors deserve separate attention: Borsh and Anchor count a `String`'s length in **bytes**, not characters, and the account for a string is allocated by byte length. A check for "no longer than N characters" on UTF-8 with multi-byte characters does not save you from overflowing the space allocated. Numeric arguments are checked for an admissible range in that same place, at the entry.

## The rules

- The length of a `String` / `Vec` is checked in **bytes** (`String::len()` is bytes), against an explicit byte limit that agrees with the account's `space`.
- Validation happens at the instruction's entry, before writing to accounts and before a CPI. Do not defer it to "it will fail during serialization" or "it will be truncated at the point of use".
- Numeric arguments get a range check (min/max, shares in bps ≤ 10000) at the entry, with a dedicated error.
- Do not invent arbitrary ceilings/limits without business sanction; but once a limit is set, it is checked explicitly rather than implied.

## Examples

### A string length in characters instead of bytes

```rust
// BAD: a limit in characters - multi-byte UTF-8 will overflow the allocated space
require!(name.chars().count() <= 32, SaleError::NameTooLong);
config.name = name;                            // space is allocated for 32 bytes, and the string is up to 128

// GOOD: a limit in bytes, agreeing with space
require!(name.len() <= MAX_NAME_BYTES, SaleError::NameTooLong);   // String::len() = bytes
config.name = name;
```

`chars().count()` counts characters; Borsh serialization and `space` operate in bytes. A string of 32 emoji is up to 128 bytes: the character check passes, while the write overflows the account or grows it beyond the rent that was paid.

### A share with no range check

```rust
// BAD: the share is taken as is
config.fee_bps = bps;                      // bps may be > 10000 (>100%)

// GOOD: the range is checked at the entry
require!(bps <= 10_000, SaleError::InvalidShare);
config.fee_bps = bps;
```

An unchecked share above 100% leaks into the calculations and yields amounts beyond what is admissible; catching that at the point of use is too late and not everywhere.
