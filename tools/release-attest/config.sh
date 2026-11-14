# Shared by attest.sh and verify.sh: the few facts that name this attestation.
ATTEST_DIR=release-attest
ATTEST_FILE=attestation.json
NAMESPACE=aibus-release-attest
IDENTITY=aibus-release-attest
KIND=aibus-release-attestation
TREE_ALGO=mktree-top-level-sans-release-attest-v2
KEY_HINT=${AIBUS_ATTEST_KEY:-$HOME/.ssh/aibus-release-attest}
# The committed public key the verifier checks against (docs/RELEASE.md).
PUBKEY_PATH=packaging/release-attest.pub
IMAGE=rust:1.89-bookworm

# name|command. These are the "every push" rows of docs/TESTING.md 'Layers', and every one has to be
# in the attestation or the verifier refuses it. Not here: benchmarks, fuzzing, the live provider
# run, and the aibus-graph-causl backend (its crate is private, see docs/BUILD.md).
SUITES='unit|cargo test --workspace --locked --lib --bins
graph|cargo test --locked -p aibus-graph
integration|cargo test --locked -p aibus-core --test integration
tui|cargo test --locked -p aibus-tui
cli|cargo test --locked -p aibus --tests
replay|cargo test --locked -p aibus-testkit --test replay --test session && cargo test --locked -p aibus-core --test replay
capability-matrix|cargo test --locked -p aibus-testkit --test capability_matrix
provider-contract|cargo test --locked -p aibus-provider-anthropic -p aibus-provider-openai -p aibus-provider-gemini
security|cargo test --locked -p aibus-core --test security && cargo test --locked -p aibus-tui --test security_commands && cargo test --locked -p aibus --test mcp_security'

# Every git that reads the tree for the attestation goes through this: replace refs (refs/replace,
# local to a clone) would swap an object's content without changing its id, so the export and the
# version would come from one tree while the hash names another, and a lazy fetch would reach the
# network for a missing object instead of failing.
git_objects() { GIT_NO_REPLACE_OBJECTS=1 GIT_NO_LAZY_FETCH=1 git "$@"; }

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }

# The id of the commit's top-level tree with the one entry named exactly release-attest dropped,
# rebuilt with `git mktree -z`. Records stay NUL-separated end to end (a file name can hold a newline
# or a tab), and the drop matches the fixed "mode type sha<TAB>name" shape, so only the top-level
# entry goes and nothing nested can hide from the hash.
tree_hash() { # repo commit
  git_objects -C "$1" ls-tree -z "$2" | perl -0ne 'print unless /\A\d+ \w+ [0-9a-f]+\trelease-attest\0\z/' | git_objects -C "$1" mktree -z
}

# Writes the exact content of a tree-ish into a directory, entry by entry from the git objects. `git
# archive` is not used on purpose: it follows the tree's own .gitattributes, so export-ignore drops
# files and export-subst rewrites them, and the copy that runs the suites would not be the tree that
# gets hashed. The tree is untrusted input and the copy runs on the host before anything is fenced, so:
#   - every path is checked first and the whole copy is refused if one has an empty, `.`, `..` or `.git`
#     component (any case, and with the invisible characters HFS+ ignores taken out);
#   - regular files and directories are written first, symlinks in a SECOND pass, and a link whose
#     path already exists is an error, so nothing is ever written through a symlink (the case-insensitive
#     `Z -> /dir` plus `z/file` shape);
#   - every file's parent has to resolve (`cd -P`) to a place inside the destination, with no symlink on
#     the way, and an existing path is never overwritten (a case collision is an error).
# Modes (644/755), symlinks and submodule directories are kept; any violation aborts with 1.
export_tree() { # repo tree-ish dir
  local ls rc
  mkdir -p "$3" || return 1
  # the list sits next to the destination: the caller's work dir, which it removes on every exit path
  ls=$(/usr/bin/mktemp "$(cd "$3" && pwd -P).ls.XXXXXX") || return 1
  [ -n "$ls" ] && [ -f "$ls" ] || return 1
  git_objects -C "$1" ls-tree -r -z "$2" >"$ls" || { rm -f "$ls"; return 1; }
  export_entries "$1" "$ls" "$3"; rc=$?
  rm -f "$ls"
  return $rc
}
# repo, a file of NUL-separated `mode type sha<TAB>path` records (git ls-tree -r -z), dest
export_entries() {
  local list=$2 dest real rec meta mode sha entry_path parent pp c bad tgt pass lastok=
  local -a comps
  mkdir -p "$3" || return 1
  dest=$(cd "$3" && pwd -P) || return 1
  # pass 0: judge every path before anything is written
  perl -0ne 'chomp; s/^[^\t]*\t//; die "export_tree: empty path\n" if $_ eq ""; for $c (split m{/}, $_, -1) { $l = $c; $l =~ s/\xe2\x80[\x8c-\x8f\xaa-\xae]|\xe2\x81[\xaa-\xaf]|\xef\xbb\xbf//g; $l = lc $l; die "export_tree: refusing the path \x27$_\x27 (component \x27$c\x27)\n" if $l eq "" || $l eq "." || $l eq ".." || $l eq ".git"; }' <"$list" || return 1
  # pass 1: files and directories. pass 2: symlinks.
  for pass in 1 2; do
    lastok=
    while IFS= read -r -d '' rec; do
      meta=${rec%%$'\t'*}; entry_path=${rec#*$'\t'}
      read -r mode _ sha <<<"$meta"
      if [ "$pass" = 2 ] && [ "$mode" != 120000 ]; then continue; fi
      parent=${entry_path%/*}; [ "$parent" != "$entry_path" ] || parent=
      if [ "$parent" != "$lastok" ] || [ -z "$parent" ]; then
      # directories are only ever made in pass 1, while the destination holds no link at all: a link
      # made earlier in pass 2 would redirect a mkdir -p to its target before the checks below could run
      if [ "$pass" = 1 ]; then mkdir -p -- "$dest/$parent" || return 1; fi
      real=$(cd -P -- "$dest/$parent" && pwd -P) || return 1
      case $real in "$dest"|"$dest"/*) ;; *) echo "export_tree: '$entry_path' resolves outside the destination" >&2; return 1 ;; esac
      pp=$dest; bad=0
      if [ -n "$parent" ]; then
        IFS=/ read -r -d '' -a comps < <(printf '%s' "$parent") || true
        for c in "${comps[@]}"; do pp=$pp/$c; if [ -L "$pp" ]; then bad=1; fi; done
      fi
      [ $bad = 0 ] || { echo "export_tree: a symlink on the way to '$entry_path'" >&2; return 1; }
      lastok=$parent
      fi
      if [ "$pass" = 1 ] && [ "$mode" = 120000 ]; then continue; fi
      case $mode in
        160000) [ -d "$dest/$entry_path" ] || mkdir -- "$dest/$entry_path" || return 1 ;;
        120000)
          [ ! -e "$dest/$entry_path" ] && [ ! -L "$dest/$entry_path" ] || { echo "export_tree: '$entry_path' already exists" >&2; return 1; }
          : ;;   # made by export_blobs once this pass has judged every link
        *)
          [ ! -e "$dest/$entry_path" ] && [ ! -L "$dest/$entry_path" ] || { echo "export_tree: '$entry_path' already exists (a case collision?)" >&2; return 1; }
          : ;;   # written by export_blobs once this pass has made every directory
      esac
    done <"$list"
    export_blobs "$1" "$list" "$dest" "$pass" || return 1
  done
}

# The blob reads and writes of one pass (1: files, 2: symlinks), in perl, which this gate already needs
# for the path filter (bash 3.2 cannot read an exact count of binary bytes). Blobs come from `git
# cat-file --batch`, 500 per process, in list order; a gitlink (160000) is never sent. The shas go in
# through a file, so no write to a pipe can block while git writes. Each reply is held to what was asked:
# the header must be `<the requested sha> blob <size>`, then exactly size bytes and a newline, and
# nothing after the last one. A missing object, a short or long stream, a bad header or a non-zero git
# exit aborts the whole copy. A symlink blob over 4096 bytes is refused from its header, before its body
# is read. The cap only bounds the read: Linux links hold at most 4095 bytes, so a target of exactly 4096
# passes this check, is read, and then fails at symlink() with ENAMETOOLONG. Files are made with
# O_EXCL|O_NOFOLLOW (an existing path is an error); before each write or link every component of the
# parent is checked to be a real directory (no symlink) that resolves inside the destination; a symlink
# target holding a NUL is refused. Every git here runs with replace refs and lazy fetch off (see
# git_objects). Path judging and the directories stay in export_entries.
export_blobs() { # repo list dest pass
  local src
  read -r -d '' src <<'PERL' || true
use strict; use warnings;
use Fcntl qw(O_WRONLY O_CREAT O_EXCL O_NOFOLLOW);
use Cwd qw(abs_path);
my ($repo, $list, $dest, $pass) = @ARGV;
my $BATCH = 500;
sub fail { print STDERR "export_tree: @_\n"; exit 1 }
open(my $lf, "<", $list) or fail("cannot read the list");
binmode $lf; local $/ = "\0";
my @ents;
while (my $rec = <$lf>) {
  chomp $rec;
  $rec =~ /\A(\d+) \w+ ([0-9a-f]+)\t(.*)\z/s or fail("malformed list record");
  my ($mode, $sha, $path) = ($1, $2, $3);
  next if $mode eq "160000";                       # a gitlink's commit is not in this repository: never sent
  next if ($pass eq "1") == ($mode eq "120000");   # pass 1 = files, pass 2 = symlinks
  push @ents, [$mode, $sha, $path];
}
close $lf;
my $um = umask; my $okparent;
sub parent_ok { # every component of the parent is a real directory (no symlink), and it resolves inside dest
  my ($path) = @_;
  (my $parent = $path) =~ s{/?[^/]*\z}{};
  return if defined $okparent && $parent eq $okparent && $pass eq "1";   # no link exists during pass 1
  my $pp = $dest;
  for my $c (split m{/}, $parent) { $pp .= "/$c"; fail("a symlink on the way to '$path'") if -l $pp; fail("'$path' has no parent directory") unless -d $pp; }
  my $real = abs_path($parent eq "" ? $dest : "$dest/$parent");
  fail("'$path' resolves outside the destination") unless defined $real && ($real eq $dest || index($real, "$dest/") == 0);
  $okparent = $parent;
}
$dest = abs_path($dest) // fail("bad destination");
while (my @batch = splice(@ents, 0, $BATCH)) {
  # the requested shas go in a file that git reads as stdin, so no write to a pipe can block while git writes
  open(my $in, "+>", undef) or fail("no scratch file");
  print $in map { "$_->[1]\n" } @batch; seek($in, 0, 0);
  my $pid = open(my $out, "-|") // fail("cannot fork");
  if (!$pid) { open(STDIN, "<&", $in) or exit 127; exec("git", "-C", $repo, "cat-file", "--batch") or exit 127; }
  binmode $out;
  for my $e (@batch) {
    my ($mode, $sha, $path) = @$e;
    my $h = do { local $/ = "\n"; scalar <$out> };
    fail("git cat-file ended early") unless defined $h && $h =~ /\n\z/ && length($h) < 200;
    $h =~ s/\n\z//;
    $h =~ /\A([0-9a-f]{40}|[0-9a-f]{64}) blob (0|[1-9][0-9]*)\z/ or fail("bad or missing object for $sha: '$h'");
    fail("object header names $1, not the requested $sha") unless $1 eq $sha;
    my $size = $2;
    fail("symlink target of '$path' is $size bytes, over 4096") if $pass eq "2" && $size > 4096;
    parent_ok($path);
    my ($fh, $body) = (undef, undef);
    if ($pass eq "1") {
      sysopen($fh, "$dest/$path", O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0666) or fail("cannot create '$path' (it may already exist, a case collision?): $!");
      binmode $fh;
    } else { $body = "" }
    my $left = $size;
    while ($left > 0) {
      my $n = read($out, my $buf, $left > 1048576 ? 1048576 : $left);
      fail("short read for $sha") unless $n;
      $left -= $n;
      if ($fh) { print $fh $buf or fail("write failed: $!") } else { $body .= $buf }
    }
    my $nlb; (read($out, $nlb, 1) // 0) == 1 && $nlb eq "\n" or fail("missing newline after $sha");
    if ($fh) {
      close $fh or fail("write failed: $!");
      chmod(0777 & ~$um, "$dest/$path") if $mode eq "100755";   # what `chmod +x` makes of a file created with 0666
    } else {
      fail("symlink target of '$path' holds a NUL") if index($body, "\0") >= 0;
      symlink($body, "$dest/$path") or fail("cannot link '$path': $!");
    }
  }
  fail("git cat-file sent more than was asked for") if read($out, my $extra, 1);
  close $out; fail("git cat-file exited with status " . ($? >> 8)) if $?;
}
PERL
  GIT_NO_REPLACE_OBJECTS=1 GIT_NO_LAZY_FETCH=1 perl -C0 -e "$src" -- "$1" "$2" "$3" "$4"
}

# The workspace version, read the way release.yml reads it.
declared_version() { # repo commit
  git_objects -C "$1" show "$2:Cargo.toml" | awk '
    /^\[/ { in_ws = ($0 == "[workspace.package]") }
    in_ws && /^version[ ]*=/ { gsub(/^[^"]*"|".*$/, ""); print; exit }'
}

# One script, the suites in order, each between a SUITE and an EXIT marker that carry a per-run random
# nonce ($1). attest.sh and tools/release-gate/e2e.sh both build their container script with this, so the
# suite list and the way it is run live in one place. The nonce stops accidental and log-embedded decoys
# (a test that prints or quotes an old log); a reader only believes a marker that carries it, and a
# suite with two markers is red. It does not stop suite code that reads the nonce out of the container's
# argv: that code runs with the same rights as the script and is out of scope. The EXIT marker starts on
# a fresh line, so output without a trailing newline cannot swallow it.
new_nonce() { od -An -N12 -tx1 /dev/urandom | tr -d ' \n'; }
lane_script() { # nonce
  echo 'set -u'
  echo 'export CARGO_TERM_COLOR=never CI=1'
  local name cmd qn qx
  qx=$(printf '%s' "$1" | sed "s/'/'\\\\''/g")
  while IFS='|' read -r name cmd; do
    # names and nonce travel as printf arguments (single-quoted), never inside a format string
    qn=$(printf '%s' "$name" | sed "s/'/'\\\\''/g")
    printf "printf '=== SUITE %%s %%s ===\\\\n' '%s' '%s'; ( %s ) 2>&1; rc=\$?; printf '\\\\n=== EXIT %%s %%s %%s ===\\\\n' '%s' \"\$rc\" '%s'\n" "$qn" "$qx" "$cmd" "$qn" "$qx"
  done <<<"$SUITES"
}
# The exit code of one suite, from the log: exactly one genuine EXIT marker or nothing.
suite_exit() { # log name nonce
  local lines
  lines=$(sed -n "s/^=== EXIT $2 \([0-9][0-9]*\) $3 ===\$/\1/p" "$1")
  [ "$(grep -c . <<<"$lines")" = 1 ] && echo "$lines"
}
