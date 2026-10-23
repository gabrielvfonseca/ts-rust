#!/usr/bin/env python3
"""gomarkers.py: check the Go line references in Rust comments against a Go tree.

usage: gomarkers.py [options] PATH...   (PATH: .rs files, or dirs to walk for .rs files)
       gomarkers.py help

Forms:
  marker  `// Go: <path>.go:<N> <Name>` (also `///` and `//!`). One line can hold
          several refs, and a short `:<N> <Name>` after a full ref uses its file.
  cont    the `//` comment lines that follow a marker line, up to the next code
          line, blank line or marker line. Refs there have the same forms.
Numbers: `N`, `N-M`, `N-:M`, `N,K`. Only the first number is checked. A bare
file name after a full path in the same comment looks in that dir first.
Names: `F`, `(*T).M`, `(t *T) M`, `T.M`, `a.b.F(args)`, in backticks or not; a
backticked snake_case name is Rust (no name). With no name after the ref, a
backticked Go name just before it counts (`` `F` (x.go:N) ``). `Name: <text>`
cites a line inside Name's body. `TestF/sub words` passes when the line holds
`"sub`. On a cont line, a plain word that the Go file does not declare is
prose: NONAME.

A ref is ok when line N of the cited Go file holds Name as a word, and the line
is not the declaration of another func or type. The file is found by path
suffix in the Go tree, then in $GOROOT/src and in the golang.org/x/sys version
of the tree's go.mod (a Rust file whose `//!` header names x/sys uses that
version). A ref followed by `at <sha>` (on its line or the next comment line,
before the next ref), or just after `at <sha>`, is checked in the tree of that
pin. When several files match, one is enough.

Kinds: ok; STALE (Name not on line N; its declarations are listed); NOTFOUND
(Name is not declared in the file); NONAME (nothing to check: by hand);
EXTERNAL (no such file, no tree for the pin, or `with #NNNN`: a PR tree).

options:
  --pin KEY   Go tree of pin KEY (UPSTREAM.json "pins"). Default: $GOPORT_PIN.
  --go DIR    Go tree (the dir that holds internal/). Overrides --pin.
  --older     for a ref that is not ok, add the newest other pin where it is ok.
  --from KEY  for a STALE or NOTFOUND ref that is ok at pin KEY, map its line to
              the current tree (git diff --histogram): MOVED N -> M when the line
              text did not change. A NONAME ref gets the mapped line as a note.
  --fix       with --from: write each MOVED number in place (not ranges).
  --tsv FILE  write every ref (ok too): file, line, form, kind, ref, name, note.
  --verbose   also print the NONAME and EXTERNAL refs.

Prints one line per ref that is not ok and a count line on stderr.
Exit 1 when a STALE, NOTFOUND or unwritten MOVED ref remains, 2 on bad arguments.
Example: GOPORT_PIN=3e9236361fa9 scripts/goport/gomarkers.py --older crates/ts_goport
"""
import glob, json, os, re, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def usage(rc):
    print(__doc__.strip(), file=sys.stderr if rc else sys.stdout)
    sys.exit(rc)


argv = sys.argv[1:]
if not argv or argv[0] in ('help', '-h', '--help'):
    usage(0 if argv else 2)
opts, paths = {}, []
i = 0
while i < len(argv):
    a = argv[i]
    if a in ('--pin', '--go', '--from', '--tsv'):
        if i + 1 >= len(argv):
            usage(2)
        opts[a] = argv[i + 1]
        i += 2
        continue
    if a in ('--older', '--fix', '--verbose'):
        opts[a] = True
    elif a.startswith('--'):
        print(f'gomarkers.py: unknown option {a}', file=sys.stderr)
        usage(2)
    else:
        paths.append(a)
    i += 1

with open(os.path.join(REPO, 'UPSTREAM.json')) as fh:
    PINS = {k: os.path.expanduser(v['goCheckout']) for k, v in json.load(fh)['pins'].items()}


def pin_dir(sha):
    """The Go tree of a pin key or commit prefix, or None."""
    for k, d in PINS.items():
        if (k.startswith(sha) or sha.startswith(k)) and os.path.isdir(d):
            return d
    for pat in (f'~/.explore/repos/microsoft__TypeScript@{sha}*/tsc',
                f'~/.explore/repos/microsoft__typescript-go@{sha[:9]}*'):
        hits = glob.glob(os.path.expanduser(pat))
        if hits:
            return hits[0]
    plain = os.path.expanduser('~/.explore/repos/microsoft__typescript-go')
    head = subprocess.run(['git', '-C', plain, 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout
    return plain if head.startswith(sha) else None


pin = opts.get('--pin') or os.environ.get('GOPORT_PIN')
go_dir = opts.get('--go') or (pin and pin_dir(pin))
if not go_dir or not os.path.isdir(go_dir) or not paths:
    print('gomarkers.py: need PATH and a Go tree (--go DIR, --pin KEY or GOPORT_PIN)', file=sys.stderr)
    usage(2)
from_dir = None
if '--from' in opts:
    from_dir = pin_dir(opts['--from'])
    if not from_dir:
        print(f'gomarkers.py: no Go tree for --from {opts["--from"]}', file=sys.stderr)
        sys.exit(2)
cur_key = next((k for k, d in PINS.items() if os.path.realpath(d) == os.path.realpath(go_dir)), None)
OLDER = [(k, d) for k, d in reversed(PINS.items()) if k != cur_key and os.path.isdir(d)]


def go_env(name):
    try:
        return subprocess.run(['go', 'env', name], capture_output=True, text=True).stdout.strip()
    except OSError:
        return ''


GOROOT, MODCACHE = go_env('GOROOT'), go_env('GOMODCACHE')


def xsys_dir(version):
    d = os.path.join(MODCACHE, f'golang.org/x/sys@{version}')
    return d if MODCACHE and os.path.isdir(d) else None


try:
    with open(os.path.join(go_dir, 'go.mod')) as fh:
        XSYS = xsys_dir(re.search(r'golang.org/x/sys (v[\w.\-]+)', fh.read()).group(1))
except (OSError, AttributeError):
    XSYS = None

# ---- Go files ----------------------------------------------------------------

indexes, cache = {}, {}


def candidates(root, path):
    if root not in indexes:
        idx = {}
        for r, dirs, files in os.walk(root):
            dirs[:] = [d for d in dirs if d not in ('.git', 'node_modules', '_submodules', 'testdata', 'vendor')]
            for f in files:
                if f.endswith('.go'):
                    idx.setdefault(f, []).append(os.path.join(r, f))
        indexes[root] = idx
    return sorted(p for p in indexes[root].get(path.rsplit('/', 1)[-1], []) if p.endswith('/' + path))


def lines_of(p):
    if p not in cache:
        with open(p, encoding='utf-8', errors='replace') as fh:
            cache[p] = fh.read().split('\n')
    return cache[p]


def decl_lines(p, ident, recv):
    """Lines that declare ident: a method of recv (or a field of type recv), else a
    func, type, var, const, `name := func`, struct field or group entry."""
    ls = lines_of(p)
    if recv:
        m = re.compile(r'^func \(\w* ?\*?' + re.escape(recv) + r'(?:\[[^\]]*\])?\) ' + re.escape(ident) + r'\b')
        out = [i for i, l in enumerate(ls, 1) if m.match(l)]
        if not out:
            start = next((i for i, l in enumerate(ls, 1) if re.match(r'^type ' + re.escape(recv) + r'\b', l)), None)
            for i in range(start + 1 if start else len(ls) + 1, len(ls) + 1):
                if ls[i - 1].startswith('}'):
                    break
                if re.match(r'^\s+' + re.escape(ident) + r'\b', ls[i - 1]):
                    out.append(i)
        if out:
            return out
    m = re.compile(r'^(?:func (?:\([^)]*\) )?' + re.escape(ident) + r'\b|(?:type|var|const) ' + re.escape(ident)
                   + r'\b|\s+' + re.escape(ident) + r' :?= func\b)')
    out = [i for i, l in enumerate(ls, 1) if m.match(l)]
    if not out:
        f = re.compile(r'^\t+' + re.escape(ident) + r'\b(?!\()')
        out = [i for i, l in enumerate(ls, 1) if f.match(l)]
    return out


def body_end(p, d):
    ls = lines_of(p)
    return next((i for i in range(d, len(ls) + 1) if ls[i - 1].startswith('}')), d)


def holds(p, n, ident, recv, stmt):
    """Line n of p holds ident (strict: not the declaration of another func or type)."""
    ls = lines_of(p)
    if n < 1 or n > len(ls):
        return False
    if stmt:
        return any(d <= n <= body_end(p, d) for d in decl_lines(p, ident, recv))
    l = ls[n - 1]
    if not re.search(r'(?<!\w)' + re.escape(ident) + r'(?!\w)', l):
        return False
    return not re.match(r'^(?:func |type )', l) or n in decl_lines(p, ident, recv)


hunks = {}


def map_line(a, b, n):
    """Line n of file a in file b: (m, True) when its text did not change, else (None, False)."""
    if (a, b) not in hunks:
        out = subprocess.run(['git', 'diff', '--no-index', '--histogram', '-U0', '--no-color', a, b],
                             capture_output=True, text=True).stdout
        hunks[(a, b)] = [(int(h.group(1)), int(h.group(2) or 1), int(h.group(3)), int(h.group(4) or 1)) for h in
                         re.finditer(r'^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@', out, re.M)]
    delta = 0
    for os_, oc, ns, nc in hunks[(a, b)]:
        if oc == 0:
            if n > os_:
                delta += nc
                continue
            break
        if n < os_:
            break
        if n < os_ + oc:
            old = lines_of(a)[n - 1].strip()
            same = [ns + k for k in range(nc) if lines_of(b)[ns + k - 1].strip() == old]
            return (same[0], True) if old and len(same) == 1 else (None, False)
        delta += nc - oc
    return n + delta, True

# ---- Rust refs ---------------------------------------------------------------


NAME = r'(?:\(\w*\s?\*?[A-Za-z_]\w*(?:\[[^\]]*\])?\)\.?\s?[A-Za-z_]\w*|[^\s,;:)]+)'
REF = re.compile(r'(?:([\w./-]*?\w+\.go)|(?<![\w.:/-])(?=:\d))(?::(\d+))((?:-:?\d+)?(?:,\d+)*)(?: (`?' + NAME + r'))?')
PIN = re.compile(r'(?<!\w)at\s+(?://\s*)?((?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40})\b')
TREE = re.compile(r'\s+with\s+#\d+')
BEFORE = re.compile(r'`([A-Za-z_][\w.()*]*)`\s*\(\s*$')
AHEAD = re.compile(r'\s*(?:[\w./-]*\.go)?:\d')
SUBTEST = re.compile(r'^`?Test\w*/([^\s`_]+)')
STOP = {'the', 'a', 'an', 'in', 'of', 'and', 'see', 'which', 'from', 'case', 'func', 'to', 'ts', 'tsgo', 'interface',
        'package', 'var', 'root', 'constants', 'lists', 'with', 'or', 'at', 'for', 'is', 'set', 'when', 'then',
        'plus', 'after', 'before', 'on', 'by', 'as', 'if', 'not', 'into', 'that', 'this', 'it', 'its', 'also',
        'makes', 'calls', 'uses', 'returns', 'where', 'line', 'lines', 'only', 'same', 'all', 'since', 'has',
        'does', 'do', 'are', 'was', 'were', 'be', 'but', 'so', 'than', 'there', 'here', 'each', 'every', 'no'}


def parse_name(name):
    """(ident, recv) of a name token, or (None, None)."""
    if not name:
        return None, None
    if name.startswith('`') and re.match(r'^`[a-z0-9]+_[a-z0-9_]*`', name):
        return None, None  # a snake_case Rust name
    name = name.strip('`')
    m = re.match(r'^\((?:\w+\s)?\*?([A-Za-z_]\w*)(?:\[[^\]]*\])?\)\.?\s?([A-Za-z_]\w*)', name) or \
        re.match(r'^([A-Za-z_]\w*)\.([A-Za-z_]\w*)(?![\w.])', name)
    if m:
        return m.group(2), m.group(1)
    m = re.match(r'^[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+', name)  # a chain `a.b.F(...)`: F
    if m:
        return m.group(0).rsplit('.', 1)[1], None
    m = re.match(r'^([A-Za-z_]\w*)', name)
    return (m.group(1), None) if m and m.group(1) not in STOP else (None, None)


headers = {}


def files_for(rf, root, path, pinned):
    """(Go files, tag) for a ref: the tree, else the Go toolchain or x/sys."""
    c = candidates(root, path)
    if c or pinned:
        return c, ''
    if rf not in headers:
        with open(rf, encoding='utf-8', errors='replace') as fh:
            headers[rf] = ''.join(l for l in fh.read().split('\n')[:40] if l.startswith('//!'))
    m = re.search(r'golang.org/x/sys\S*\s*\((v[\d.]+)', headers[rf])
    xs = (m and xsys_dir(m.group(1))) or XSYS
    xc = candidates(xs, path) if xs else []
    if xc and 'x/sys' in headers[rf]:
        return xc, '@xsys'
    sc = candidates(os.path.join(GOROOT, 'src'), path) if GOROOT else []
    if sc:
        return sc, '@stdlib'
    return (xc, '@xsys') if xc else ([], '')


def rust_files(ps):
    for p in ps:
        if os.path.isdir(p):
            for r, dirs, files in os.walk(p):
                dirs.sort()
                for f in sorted(files):
                    if f.endswith('.rs'):
                        yield os.path.join(r, f)
        else:
            yield p


def comment_lines(src):
    """(index, form) of each comment line with refs to check: marker lines and their continuation."""
    form = None
    for idx, line in enumerate(src):
        s = line.strip()
        if re.match(r'\s*//[/!]?\s*Go:', line):
            form = 'marker'
        elif form and s.startswith('//'):
            form = 'cont'
        else:
            form = None
        if form:
            yield idx, form


def pin_for(ctx, line, refs, k):
    """The `at <sha>` of ref k: one just before it (`at <sha> :N`), else the first one
    after it, before the next ref (on the next comment line: before any ref there)."""
    r = refs[k]
    for p in PIN.finditer(ctx):
        if p.end() <= r.start() and not ctx[p.end():r.start()].strip():
            return p
    for p in PIN.finditer(ctx):
        if p.start() <= r.start() or AHEAD.match(ctx, p.end()):
            continue
        if k + 1 < len(refs) and p.start() > refs[k + 1].start():
            return None
        if p.start() > len(line) and re.search(r'\.go:\d|(?<![\w.:/-]):\d', ctx[len(line):p.start()]):
            return None
        return p
    return None


def ok_in(files, n, ident, recv, stmt, sub):
    """Line n of one of the files holds the name (a subtest: its name in a string)."""
    for f in files:
        if holds(f, n, ident, recv, stmt):
            return True
        if sub and n <= len(lines_of(f)) and '"' + sub in lines_of(f)[n - 1]:
            return True
    return False


counts, rows, bad = {}, [], 0


def report(rf, idx, form, kind, ref, name, note=''):
    global bad
    counts[(form, kind)] = counts.get((form, kind), 0) + 1
    rows.append((rf, str(idx + 1), form, kind, ref, name or '', note))
    if kind in ('STALE', 'NOTFOUND', 'MOVED'):
        bad += 1
    if kind != 'ok' and (kind in ('STALE', 'NOTFOUND', 'MOVED') or '--verbose' in opts):
        print(f'{rf}:{idx + 1}: {kind} {ref} {name or ""}{f" ({note})" if note else ""}'.replace('  ', ' '))


for rf in rust_files(paths):
    with open(rf, encoding='utf-8', errors='replace') as fh:
        src = fh.read().split('\n')
    edits = []
    cur = None  # the file of the last full ref in this comment block
    for idx, form in comment_lines(src):
        line = src[idx]
        if form == 'marker':
            cur = None
        ctx = line
        if idx + 1 < len(src) and re.match(r'^\s*//(?! Go:)', src[idx + 1]):
            ctx += ' ' + src[idx + 1].strip()
        start = line.find('// Go:') + 6 if form == 'marker' else line.find('//') + 2
        refs = list(REF.finditer(line, start))
        for k, r in enumerate(refs):
            path = r.group(1) or cur
            if not path:
                continue
            if r.group(1) and '/' not in path and cur and '/' in cur:
                # a bare file name in the block of a full path: that dir first
                near = cur.rsplit('/', 1)[0] + '/' + path
                if candidates(go_dir, near) or files_for(rf, go_dir, near, False)[0]:
                    path = near
            if r.group(1):
                cur = path
            n, rng, name = int(r.group(2)), r.group(3), r.group(4)
            pm = pin_for(ctx, line, refs, k)
            root, tag = go_dir, ''
            if pm:
                root, tag = pin_dir(pm.group(1)), '@' + pm.group(1)
            ref = f'{path}:{n}{rng}{tag}'
            if root is None:
                report(rf, idx, form, 'EXTERNAL', ref, name, f'no tree for pin {pm.group(1)}')
                continue
            if TREE.match(line, r.end(3)):
                report(rf, idx, form, 'EXTERNAL', ref, name, 'PR tree')
                continue
            files, extra = files_for(rf, root, path, bool(pm))
            ref += extra
            if not files:
                report(rf, idx, form, 'EXTERNAL', ref, name, 'no such Go file')
                continue
            ident, recv = parse_name(name)
            sm = SUBTEST.match(name or '')
            sub = sm.group(1) if sm else None
            stmt = bool(name) and r.end(4) < len(line) and line[r.end(4)] == ':'
            if ident is None:
                bm = BEFORE.search(line, 0, r.start())
                if bm and '_' not in bm.group(1):  # a snake_case name is a Rust name
                    name = f'`{bm.group(1)}`'
                    ident, recv = parse_name(bm.group(1))
            if ident is None:
                note = ''
                if from_dir and not pm and not extra:
                    for a in candidates(from_dir, path):
                        b = os.path.join(root, os.path.relpath(a, from_dir))
                        if os.path.exists(b):
                            m, same = map_line(a, b, n)
                            note = f'if written at {opts["--from"]}: ' + (f'-> {m}' if same else 'line changed')
                            break
                report(rf, idx, form, 'NONAME', ref, name, note)
                continue
            if ok_in(files, n, ident, recv, stmt, sub):
                report(rf, idx, form, 'ok', ref, name)
                continue
            decls = sorted({(f, d) for f in files for d in decl_lines(f, ident, recv)})
            if not decls and form == 'cont' and not name.startswith('`'):
                report(rf, idx, form, 'NONAME', ref, name, 'a word, not a name of the file')
                continue
            kind = 'STALE' if decls else 'NOTFOUND'
            notes = []
            if decls:
                notes.append('decls ' + ', '.join(f'{f.rsplit("/", 1)[-1]}:{d}' for f, d in decls[:6]))
            if from_dir and not pm and not extra:
                for a in candidates(from_dir, path):
                    b = os.path.join(root, os.path.relpath(a, from_dir))
                    if not (os.path.exists(b) and ok_in([a], n, ident, recv, stmt, sub)):
                        continue
                    m, same = map_line(a, b, n)
                    if same and ok_in([b], m, ident, recv, stmt, sub):
                        kind = 'MOVED'
                        notes.insert(0, f'-> {m}')
                        if '--fix' in opts and not rng:
                            edits.append((idx, r.start(2), r.end(2), str(m)))
                    else:
                        notes.insert(0, f'ok at {opts["--from"]}, line changed')
                    break
            if '--older' in opts and kind != 'MOVED' and not pm and not extra:
                for key, d in OLDER:
                    if ok_in(candidates(d, path), n, ident, recv, stmt, sub):
                        notes.append(f'ok at {key}')
                        break
                else:
                    notes.append('ok at no pin')
            report(rf, idx, form, kind, ref, name, '; '.join(notes))
    if edits:
        for idx, s, e, new in sorted(edits, key=lambda x: (x[0], -x[1])):
            src[idx] = src[idx][:s] + new + src[idx][e:]
        with open(rf, 'w', encoding='utf-8') as fh:
            fh.write('\n'.join(src))
        bad -= len(edits)

if '--tsv' in opts:
    with open(opts['--tsv'], 'w') as fh:
        fh.write('file\tline\tform\tkind\tref\tname\tnote\n')
        for row in rows:
            fh.write('\t'.join(row) + '\n')
print(' '.join(f'{f}.{k}={v}' for (f, k), v in sorted(counts.items())), file=sys.stderr)
sys.exit(1 if bad else 0)
