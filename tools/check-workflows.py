#!/usr/bin/env python3
"""Static safety checks for .github/workflows/*.yml (S3-7). Stdlib + PyYAML (present on ubuntu runners).

Run:  python3 -I tools/check-workflows.py [workflow.yml ...]   (default: every file in .github/workflows)
Tests: python3 -I tools/check-workflows_test.py

Asserts (fail closed: a parse problem is an error):
  * no `pull_request_target` trigger;
  * the only secret any workflow may name is secrets.GITHUB_TOKEN;
  * every `uses:` is pinned: a full commit SHA, a version tag (vN, vN.N, vN.N.N) or, as ci.yml already does,
    dtolnay/rust-toolchain@stable; local `./` actions are allowed; docker:// and branch refs are not;
  * top-level `permissions` is read-only; `write` appears only in the one job that creates a release, and only as
    `contents: write`;
  * release.yml: every `gh release create` has a standalone `--draft` argument (command lines are tokenised, quotes
    respected), nothing sets draft false, edits/publishes a release (gh release edit/upload/delete, gh api on releases,
    release actions, indirect `$CMD create`), the publishing job's `if` is EXACTLY
    `github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')` (after whitespace/`${{ }}` normalisation),
    and no signing material is named;
  * no workflow interpolates github.ref_name / head_ref / ref / event.* inside `run:` text (use env:);
  * WARNING (not an error): `uses:` in release.yml that are not SHA-pinned (see docs/RELEASING.md, residual risks).
"""
import re
import shlex
import sys
from pathlib import Path

try:
    import yaml
except ImportError:  # fail closed: do not pretend to validate without a parser
    sys.stderr.write("check-workflows: PyYAML is required (python3-yaml)\n")
    sys.exit(2)

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
VERSION_RE = re.compile(r"^v\d+(\.\d+){0,2}$")
FLOATING_OK = {"dtolnay/rust-toolchain@stable"}
EXPR_RE = re.compile(r"\$\{\{(.*?)\}\}", re.S)
SECRET_RE = re.compile(r"\bsecrets\b(?:\s*\.\s*([A-Za-z0-9_]+))?", re.I)
SIGNING_RE = re.compile(
    r"signtool|certificate|\.pfx|\.p12|codesign|authenticode|cosign|gpg\s+--(sign|detach|armor)|--sign\b", re.I
)
# draft set to anything but a literal true: draft=false, draft: false, "draft": false, draft: ${{ ... }}, draft=$VAR
DRAFT_FALSE_RE = re.compile(r"\bdraft['\"]?\s*[:=]\s*['\"]?(false|0|no|off|\$)", re.I)
GH_CREATE_RE = re.compile(r"\bgh\b[^\n;&|]*?\brelease\s+create\b")
RELEASE_ACTION_RE = re.compile(r"release", re.I)  # any action whose owner/repo name says release
INJECT_RE = re.compile(
    r"github\s*\.\s*(ref_name|head_ref|base_ref|ref|event)\b|github\s*\[|toJSON\s*\(\s*github\s*\)", re.I
)
PUBLISH_COND = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
ALLOWED_RELEASE_ACTIONS = {"create", "view", "list", "download"}
SHELL_PUNCT = set("();<>|&")


def _norm_cond(cond):
    c = " ".join(str(cond).split())
    m = re.fullmatch(r"\$\{\{(.*)\}\}", c)
    if m:
        c = m.group(1).strip()
    return re.sub(r"\s*,\s*", ", ", c)


def _strip_comments(text):
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))


def _logical_lines(text):
    """Join shell line continuations so a multi-line command is one line."""
    return re.sub(r"\\\s*\n\s*", " ", text).splitlines()


def _on(doc):
    # PyYAML (YAML 1.1) parses the key `on` as boolean True.
    return doc.get("on", doc.get(True))


def _triggers(doc):
    on = _on(doc)
    if isinstance(on, dict):
        return set(on)
    if isinstance(on, list):
        return set(on)
    return {on} if on else set()


def _walk_uses(doc):
    for jname, job in (doc.get("jobs") or {}).items():
        if not isinstance(job, dict):
            continue
        if "uses" in job:
            yield jname, job["uses"]
        for step in job.get("steps") or []:
            if isinstance(step, dict) and "uses" in step:
                yield jname, step["uses"]


def _job_text(job):
    return yaml.safe_dump(job, width=10**6)


def _runs(job):
    """The `run:` strings of a job (comment lines dropped)."""
    out = []
    for step in (job.get("steps") or []) if isinstance(job, dict) else []:
        if isinstance(step, dict) and "run" in step:
            out.append(_strip_comments(str(step["run"])))
    return out


def _segments(line):
    """Tokenise one logical shell line (quotes kept on tokens, so a quoted "--draft" is not the flag) and split it
    into simple commands at ; && || | ( ). Raises ValueError on unbalanced quotes."""
    lex = shlex.shlex(line, posix=False, punctuation_chars=True)
    lex.whitespace_split = True
    lex.commenters = ""
    segs, cur = [], []
    for t in lex:
        if t and set(t) <= SHELL_PUNCT:
            if cur:
                segs.append(cur)
            cur = []
        else:
            cur.append(t)
    if cur:
        segs.append(cur)
    return segs


def _check_commands(name, runs):
    """Command-level release rules for every run: text. Returns error strings."""
    errs = []
    for run in runs:
        for line in _logical_lines(run):
            if not re.search(r"\b(gh|release|create)\b", line):
                continue
            try:
                segs = _segments(line)
            except ValueError as e:
                errs.append(f"{name}: cannot tokenise a line that mentions gh/release/create ({e}): {line.strip()[:80]}")
                continue
            release_seen = False
            for seg in segs:
                i = 0
                while i < len(seg) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", seg[i]):
                    i += 1  # leading VAR=value assignments
                seg = seg[i:]
                if not seg:
                    continue
                cmd, args = seg[0], seg[1:]
                if cmd != "gh":
                    if re.search(r"[$`]", cmd) and any(a in ("create", "release", "edit", "upload", "delete") for a in args):
                        errs.append(f"{name}: indirect release command ({cmd} ...): {line.strip()[:80]}")
                    continue
                pos, skip = [], False
                for a in args:
                    if skip:
                        skip = False
                    elif a in ("-R", "--repo"):
                        skip = True  # global flag with a separate value
                    elif not a.startswith("-"):
                        pos.append(a)
                sub = pos[0] if pos else ""
                if sub == "api":
                    if any("release" in a.lower() for a in args):
                        errs.append(f"{name}: gh api touching releases: {line.strip()[:80]}")
                elif sub == "release":
                    release_seen = True
                    action = pos[1] if len(pos) > 1 else ""
                    if action not in ALLOWED_RELEASE_ACTIONS:
                        errs.append(f"{name}: edits or deletes an existing release (gh release {action or '?'}): {line.strip()[:80]}")
                    elif action == "create" and "--draft" not in args:
                        errs.append(f"{name}: gh release create without --draft: {line.strip()[:80]}")
            if (
                re.search(r"\bgh\b", line)
                and re.search(r"\brelease\b", line)
                and not release_seen
                and not any(e.endswith(line.strip()[:80]) for e in errs)
            ):
                errs.append(f"{name}: obscured gh release command (quoted, indirect or unparsed): {line.strip()[:80]}")
    return errs


def non_sha_uses(name, text):
    """Warnings: `uses:` that are not pinned to a full commit SHA (release workflow only)."""
    try:
        doc = yaml.safe_load(text)
    except yaml.YAMLError:
        return []
    out = []
    if isinstance(doc, dict) and isinstance(doc.get("jobs"), dict):
        for jname, uses in _walk_uses(doc):
            u = str(uses)
            if not u.startswith("./") and not SHA_RE.match(u.rsplit("@", 1)[-1]):
                out.append(f"WARNING: {name}: job {jname}: {u} is not SHA-pinned (docs/RELEASING.md, residual risks)")
    return out


def check_workflow(name, text):
    """Return a list of error strings for one workflow file."""
    errs = []
    try:
        doc = yaml.safe_load(text)
    except yaml.YAMLError as e:
        return [f"{name}: YAML does not parse: {e}"]
    if not isinstance(doc, dict) or not isinstance(doc.get("jobs"), dict) or not doc["jobs"]:
        return [f"{name}: no jobs"]
    code = _strip_comments(text)

    if "pull_request_target" in _triggers(doc) or "pull_request_target" in code:
        errs.append(f"{name}: pull_request_target is forbidden")

    for expr in EXPR_RE.finditer(code):  # only inside ${{ }} expressions (prose may say "no secrets")
        for m in SECRET_RE.finditer(expr.group(1)):
            if (m.group(1) or "") != "GITHUB_TOKEN":
                errs.append(f"{name}: references a secret other than secrets.GITHUB_TOKEN ({m.group(0).strip()!r})")
    for jname, job in doc["jobs"].items():
        if isinstance(job, dict) and "secrets" in job:
            errs.append(f"{name}: job {jname}: passes secrets to a reusable workflow")

    for jname, uses in _walk_uses(doc):
        u = str(uses)
        if u.startswith("./"):
            continue
        if RELEASE_ACTION_RE.search(u.split("@", 1)[0]) and "@" not in u:
            errs.append(f"{name}: job {jname}: release action {u!r} is forbidden")
        if "@" not in u or u.startswith("docker://"):
            errs.append(f"{name}: job {jname}: unpinned or docker action {u!r}")
            continue
        if RELEASE_ACTION_RE.search(u.rsplit("@", 1)[0]):
            errs.append(f"{name}: job {jname}: release action {u!r} is forbidden (use the checked gh release create --draft)")
        ref = u.rsplit("@", 1)[1]
        if not (SHA_RE.match(ref) or VERSION_RE.match(ref) or u in FLOATING_OK):
            errs.append(f"{name}: job {jname}: action {u!r} is not pinned to a SHA or version tag")

    top = doc.get("permissions")
    if top != {"contents": "read"}:
        errs.append(f"{name}: top-level permissions must be exactly {{contents: read}}, got {top!r}")

    for jname, job in doc["jobs"].items():
        for run in _runs(job):
            for expr in EXPR_RE.finditer(run):
                if INJECT_RE.search(expr.group(1)):
                    errs.append(
                        f"{name}: job {jname}: `${{{{{expr.group(1).strip()}}}}}` inside run: text (script injection); pass it through env:"
                    )

    publishers = []
    for jname, job in doc["jobs"].items():
        perms = job.get("permissions") if isinstance(job, dict) else None
        jtext = "\n".join(_logical_lines("\n".join(_runs(job))))
        creates = bool(GH_CREATE_RE.search(jtext))
        if creates:
            publishers.append(jname)
        if perms is None:
            continue
        if not isinstance(perms, dict):
            errs.append(f"{name}: job {jname}: permissions must be a mapping, got {perms!r}")
            continue
        writes = {k: v for k, v in perms.items() if v != "read" and v != "none"}
        if writes:
            if not creates:
                errs.append(f"{name}: job {jname}: write permission {writes} outside the release-creating job")
            elif writes != {"contents": "write"}:
                errs.append(f"{name}: job {jname}: only contents: write is allowed, got {writes}")
    for jname in publishers:
        job = doc["jobs"][jname]
        if (job.get("permissions") or {}).get("contents") != "write":
            errs.append(f"{name}: job {jname} creates a release but lacks contents: write")
        cond = job.get("if", "")
        if _norm_cond(cond) != PUBLISH_COND:
            errs.append(f"{name}: job {jname} must run only for a v* tag push: if must be exactly {PUBLISH_COND!r}, got {cond!r}")

    is_release = name.endswith("release.yml")
    if is_release or "gh release" in code:
        errs += _check_commands(name, [r for j in doc["jobs"].values() for r in _runs(j)])
        if DRAFT_FALSE_RE.search(code):
            errs.append(f"{name}: sets draft to false")
    if is_release:
        if not publishers:
            errs.append(f"{name}: no gh release create step (check is vacuous)")
        if len(publishers) > 1:
            errs.append(f"{name}: more than one release-creating job: {publishers}")
        for m in SIGNING_RE.finditer(code):
            errs.append(f"{name}: signing material named in the release workflow: {m.group(0)!r}")
    return errs


def main(argv):
    root = Path(__file__).resolve().parent.parent
    files = [Path(a) for a in argv] or sorted((root / ".github" / "workflows").glob("*.yml"))
    if not files:
        print("check-workflows: no workflow files found")
        return 1
    errs, warns = [], []
    for f in files:
        text = f.read_text(encoding="utf-8")
        errs += check_workflow(f.name, text)
        if f.name == "release.yml":
            warns += non_sha_uses(f.name, text)
    if warns:
        print("\n".join(warns))
    names = {f.name for f in files}
    if not argv and "release.yml" not in names:
        errs.append("release.yml is missing")
    if errs:
        print("\n".join(errs))
        return 1
    print(f"check-workflows: OK ({len(files)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
