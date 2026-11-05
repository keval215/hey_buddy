"""Unit tests for tools/check-workflows.py. Run: python3 -I tools/check-workflows_test.py"""
import importlib.util
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("check_workflows", HERE / "check-workflows.py")
cw = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cw)

GOOD = """
name: Release
on:
  push:
    tags: ['v*']
  pull_request:
permissions:
  contents: read
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/upload-artifact@0123456789abcdef0123456789abcdef01234567
      - run: echo hi
  publish:
    needs: [build]
    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          gh release create "$TAG" --draft \\
            --title x assets/*
"""


def errs(text, name="release.yml"):
    return cw.check_workflow(name, text)


def has(text, needle, name="release.yml"):
    return any(needle in e for e in errs(text, name))


class Positive(unittest.TestCase):
    def test_good_fixture_is_clean(self):
        self.assertEqual(errs(GOOD), [])

    def test_real_workflows_are_clean(self):
        root = HERE.parent / ".github" / "workflows"
        files = sorted(root.glob("*.yml"))
        self.assertIn("release.yml", [f.name for f in files])  # not vacuous
        self.assertIn("ci.yml", [f.name for f in files])
        for f in files:
            self.assertEqual(cw.check_workflow(f.name, f.read_text(encoding="utf-8")), [], f.name)

    def test_multiline_draft_is_found(self):
        self.assertEqual(errs(GOOD), [])  # --draft sits on the first physical line of a continued command


class Negative(unittest.TestCase):
    def test_missing_draft(self):
        self.assertTrue(has(GOOD.replace("--draft", ""), "without --draft"))

    def test_draft_false_flag(self):
        self.assertTrue(has(GOOD.replace("--draft", "--draft=false"), "draft"))

    def test_draft_false_yaml(self):
        self.assertTrue(has(GOOD + "      - uses: x/y@v1\n        with:\n          draft: false\n", "draft to false"))

    def test_release_edit(self):
        self.assertTrue(has(GOOD + "          gh release edit v1 --draft=false\n", "edits"))

    def test_other_secret(self):
        self.assertTrue(has(GOOD.replace("secrets.GITHUB_TOKEN", "secrets.PAT"), "secret"))
        self.assertTrue(has(GOOD.replace("secrets.GITHUB_TOKEN", "secrets['X']"), "secret"))
        self.assertTrue(has(GOOD.replace("secrets.GITHUB_TOKEN", "toJSON(secrets) }}{{ x"), "secret"))

    def test_secret_in_other_workflow(self):
        t = "on: push\npermissions:\n  contents: read\njobs:\n  a:\n    runs-on: x\n    steps:\n      - run: echo ${{ secrets.K }}\n"
        self.assertTrue(has(t, "secret", "ci.yml"))

    def test_pull_request_target(self):
        self.assertTrue(has(GOOD.replace("  pull_request:\n", "  pull_request_target:\n"), "pull_request_target"))

    def test_write_outside_publish_job(self):
        t = GOOD.replace("  build:\n    runs-on: ubuntu-latest\n", "  build:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n")
        self.assertTrue(has(t, "outside the release-creating job"))

    def test_other_write_in_publish_job(self):
        t = GOOD.replace("      contents: write\n", "      contents: write\n      id-token: write\n")
        self.assertTrue(has(t, "only contents: write"))

    def test_top_level_write(self):
        self.assertTrue(has(GOOD.replace("permissions:\n  contents: read\njobs", "permissions: write-all\njobs"), "top-level"))
        self.assertTrue(has(GOOD.replace("permissions:\n  contents: read\njobs", "jobs"), "top-level"))

    def test_publish_not_tag_gated(self):
        t = GOOD.replace("if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')", "if: always()")
        self.assertTrue(has(t, "v* tag push"))
        self.assertTrue(has(GOOD.replace("    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')\n", ""), "v* tag push"))

    def test_unpinned_actions(self):
        for bad in ["actions/checkout@main", "actions/checkout", "actions/checkout@v4-beta", "docker://alpine:3", "a/b@abc123", "dtolnay/rust-toolchain@master"]:
            self.assertTrue(has(GOOD.replace("actions/checkout@v4", bad), "pinned"), bad)

    def test_signing_strings(self):
        for bad in ["signtool sign /f a.pfx", "echo certificate", "x.pfx", "codesign -s -", "Set-AuthenticodeSignature"]:
            self.assertTrue(has(GOOD.replace("echo hi", bad), "signing"), bad)

    def test_signing_only_checked_for_release(self):
        t = GOOD.replace("echo hi", "echo no signing and no certificate here").replace("gh release create", "echo")
        self.assertFalse(has(t, "signing", "ci.yml"))

    def test_no_release_step_is_vacuous(self):
        t = GOOD.replace("gh release create", "echo")
        self.assertTrue(has(t, "vacuous"))

    def test_two_publish_jobs(self):
        extra = "  publish2:\n    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')\n    runs-on: x\n    permissions:\n      contents: write\n    steps:\n      - run: gh release create a --draft\n"
        self.assertTrue(has(GOOD + extra, "more than one"))

    def test_bad_yaml(self):
        self.assertTrue(has("a: [", "does not parse"))
        self.assertTrue(has("name: x\n", "no jobs"))

    def test_comments_do_not_count(self):
        self.assertEqual(errs(GOOD + "# secrets.PAT and certificate are not used\n"), [])


if __name__ == "__main__":
    unittest.main()
