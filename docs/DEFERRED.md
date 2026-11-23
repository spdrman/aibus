# Deferred work

Scope that was cut from an issue on its way to closing, with the reason and what picks it up. One
section per issue.

## #368 Protected-path rule misses /private/var on macOS

I fixed the protected-path rule and the path-tool specifiers so they compare resolved paths. Two
gaps came up in review that were already there before this change, and I left them alone:

- **Case-insensitive filesystems.** The protected-home check (`starts_with`) and the glob match are
  case-sensitive, so on APFS (or any case-insensitive volume) a write to `~/.CONFIG/AIBUS/x` that
  creates new directories, or to `/tmp/OUT/x` against a deny on `/tmp/out/**`, isn't caught by the
  rule. Fixing it means folding case per volume (or comparing against what the filesystem reports
  for the existing part of the path), which is its own change.
- **A rule that names a symlinked file.** A rule with no glob that names a symlink to a file, say
  `write(/tmp/link)`, only resolves its directory, never its last component, so it can't meet the
  canonical subject (the link's target). Resolving the last component too would let the link's
  owner move where the rule points, so it needs the same owner check the allow side has.
