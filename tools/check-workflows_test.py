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
        COND = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
        for bad in [
            "github.event_name == 'push' || startsWith(github.ref, 'refs/tags/v')",
            "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') || always()",
            "always() && github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')",
            "${{ always() }} && github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')",
            "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') && !cancelled()",
            "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') && failure()",
            "github.event_name == 'push' && cancelled() && startsWith(github.ref, 'refs/tags/v')",
            "${{ !(github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')) }}",
            "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/')",
            "github.event_name != 'push' && startsWith(github.ref, 'refs/tags/v')",
            "true",
        ]:
            self.assertTrue(has(GOOD.replace(COND, bad), "v* tag push"), bad)

    def test_publish_gate_normalisation_is_accepted(self):
        COND = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
        for ok in [
            "${{ github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') }}",
            "github.event_name  ==  'push'   &&  startsWith(github.ref ,  'refs/tags/v')",
            "github.event_name == 'push' && startsWith(github.ref,'refs/tags/v')",
        ]:
            self.assertEqual(errs(GOOD.replace(COND, ok)), [], ok)

    def test_publish_gate_missing(self):
        self.assertTrue(has(GOOD.replace("    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')\n", ""), "v* tag push"))

    def _with_cmd(self, new):
        """GOOD with the publish command replaced by `new` (the create command plus --draft is replaced entirely)."""
        old = 'gh release create "$TAG" --draft \\\n            --title x assets/*'
        self.assertIn(old, GOOD)
        return GOOD.replace(old, new)

    def test_draft_only_inside_quoted_argument(self):
        self.assertTrue(has(self._with_cmd('gh release create "$TAG" --title "--draft" assets/*'), "without --draft"))
        self.assertTrue(has(self._with_cmd("gh release create \"$TAG\" --notes '--draft' assets/*"), "without --draft"))
        self.assertTrue(has(self._with_cmd('gh release create "$TAG" --title="x --draft" assets/*'), "without --draft"))

    def test_draft_flag_forms_accepted(self):
        self.assertEqual(errs(self._with_cmd('gh   release   create "$TAG" --title x --draft assets/*')), [])
        self.assertEqual(errs(self._with_cmd('gh -R o/r release create "$TAG" --title x --draft assets/*')), [])

    def test_draft_on_other_command_does_not_count(self):
        self.assertTrue(has(self._with_cmd('echo --draft && gh release create "$TAG" assets/*'), "without --draft"))
        self.assertTrue(has(self._with_cmd('gh release create "$TAG" assets/* ; echo --draft'), "without --draft"))

    def test_spacing_variants_of_create(self):
        t = self._with_cmd('gh  release  create "$TAG" assets/*')
        self.assertTrue(has(t, "without --draft"))
        t = self._with_cmd('gh -R o/r release create "$TAG" assets/*')
        self.assertTrue(has(t, "without --draft"))
        t = self._with_cmd('gh --repo o/r release create "$TAG" assets/*')
        self.assertTrue(has(t, "release"))  # fail closed even if the parser cannot tell

    def test_indirect_command(self):
        t = self._with_cmd('CMD="gh release"\n          $CMD create "$TAG" assets/*')
        self.assertTrue(has(t, "indirect"))
        t = self._with_cmd('"$GH" release create "$TAG" --draft assets/*')
        self.assertTrue(has(t, "indirect"))

    def test_obscured_command(self):
        self.assertTrue(has(self._with_cmd('bash -c "gh release create $TAG --draft"'), "obscured"))
        self.assertTrue(has(self._with_cmd('eval "gh release create $TAG"'), "obscured"))

    def test_gh_api_on_releases(self):
        for cmd in [
            "gh api -X PATCH repos/o/r/releases/1 -F draft=false",
            "gh api --method PATCH /repos/o/r/releases/1 --field draft=false",
            "gh api repos/o/r/releases -f tag_name=v1",
            "gh api -X PATCH repos/o/r/RELEASES/1",
        ]:
            self.assertTrue(has(GOOD + "          " + cmd + "\n", "gh api"), cmd)

    def test_draft_false_forms(self):
        for bad in ["-F draft=false", "--field draft=0", 'draft: "false"', "draft: ${{ inputs.d }}", "'draft': false", "draft=$D", "draft: 'false'"]:
            self.assertTrue(has(GOOD + "          echo x " + bad + "\n", "draft to false"), bad)
        self.assertEqual(errs(GOOD + "          echo draft=true\n"), [])

    def test_release_actions(self):
        for bad in ["softprops/action-gh-release@v2", "ncipollo/release-action@v1", "actions/create-release@v1", "actions/upload-release-asset@v1"]:
            self.assertTrue(has(GOOD.replace("actions/checkout@v4", bad), "release action"), bad)
        self.assertTrue(has(GOOD.replace("actions/checkout@v4", "softprops/action-gh-release@v2"), "release action", "ci.yml"))

    def test_release_other_subcommands(self):
        for sub in ["edit v1 --draft=false", "upload v1 f", "delete v1", "upload v1 f --clobber"]:
            self.assertTrue(has(GOOD + "          gh release " + sub + "\n", "existing release"), sub)

    def test_unbalanced_quote_fails_closed(self):
        self.assertTrue(has(self._with_cmd('gh release create "$TAG --draft assets/*'), "tokenise"))

    def test_injection_in_run(self):
        for expr in ["github.ref_name", "github.head_ref", "github.ref", "github.event.pull_request.title", "github.event.head_commit.message",
                     "github['ref_name']", "toJSON(github)", "github.base_ref", " github.ref_name "]:
            t = GOOD.replace("echo hi", "echo ${{ " + expr + " }}")
            self.assertTrue(has(t, "script injection"), expr)
            self.assertTrue(has(t, "script injection", "ci.yml"), expr)

    def test_injection_via_env_is_fine(self):
        t = GOOD.replace("      - run: echo hi\n", "      - env:\n          TAG: ${{ github.ref_name }}\n        run: echo \"$TAG\"\n")
        self.assertEqual(errs(t), [])
        self.assertEqual(errs(GOOD.replace("echo hi", "echo ${{ github.repository }} ${{ runner.os }}")), [])

    def test_warns_about_non_sha_uses(self):
        w = cw.non_sha_uses("release.yml", GOOD)
        self.assertEqual(len(w), 2)  # checkout@v4 and rust-toolchain@stable; the 40-hex upload-artifact is fine
        self.assertTrue(all(x.startswith("WARNING") for x in w))
        self.assertEqual(errs(GOOD), [])  # a warning is not an error

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
