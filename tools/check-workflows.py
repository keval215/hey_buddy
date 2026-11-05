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
  * release.yml: every `gh release create` has --draft, nothing sets draft false or edits/publishes a release, the
    publishing job only runs for a v* tag push, and no signing material is named.
"""
import re
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
DRAFT_FALSE_RE = re.compile(r"--draft\s*=\s*false|\bdraft\s*:\s*['\"]?false", re.I)


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
        if "@" not in u or u.startswith("docker://"):
            errs.append(f"{name}: job {jname}: unpinned or docker action {u!r}")
            continue
        ref = u.rsplit("@", 1)[1]
        if not (SHA_RE.match(ref) or VERSION_RE.match(ref) or u in FLOATING_OK):
            errs.append(f"{name}: job {jname}: action {u!r} is not pinned to a SHA or version tag")

    top = doc.get("permissions")
    if top != {"contents": "read"}:
        errs.append(f"{name}: top-level permissions must be exactly {{contents: read}}, got {top!r}")

    publishers = []
    for jname, job in doc["jobs"].items():
        perms = job.get("permissions") if isinstance(job, dict) else None
        jtext = "\n".join(_logical_lines(_strip_comments(_job_text(job)))) if isinstance(job, dict) else ""
        creates = "gh release create" in jtext
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
        cond = str(job.get("if", ""))
        if "refs/tags/v" not in cond or "'push'" not in cond:
            errs.append(f"{name}: job {jname} must run only for a v* tag push (if: {cond!r})")

    is_release = name.endswith("release.yml")
    if is_release or "gh release" in code:
        for line in _logical_lines(code):
            if "gh release create" in line and "--draft" not in line:
                errs.append(f"{name}: gh release create without --draft: {line.strip()[:80]}")
        if DRAFT_FALSE_RE.search(code):
            errs.append(f"{name}: sets draft to false")
        if re.search(r"gh\s+release\s+(edit|upload\s+--clobber|delete)", code):
            errs.append(f"{name}: edits or deletes an existing release")
        if re.search(r"--draft(\s|$)", code) is None and is_release:
            errs.append(f"{name}: no --draft found")
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
    errs = []
    for f in files:
        errs += check_workflow(f.name, f.read_text(encoding="utf-8"))
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
