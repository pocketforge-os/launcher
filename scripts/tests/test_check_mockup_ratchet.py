import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "check-mockup-ratchet.py"


def source(rows):
    body = "\n".join(f'    ("{name}", "{name}.png", {value}),' for name, value in rows)
    return f'const SCREENS: [(&str, &str, f64); {len(rows)}] = [\n{body}\n];\n'


class RatchetCheckTests(unittest.TestCase):
    def run_check(self, old_rows, new_rows, *, old_waivers="", new_waivers=""):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {
                "old.rs": source(old_rows),
                "new.rs": source(new_rows),
                "old.md": old_waivers,
                "new.md": new_waivers,
            }
            for name, contents in files.items():
                (root / name).write_text(contents)
            return subprocess.run(
                [
                    "python3", str(SCRIPT),
                    "--base-thresholds", str(root / "old.rs"),
                    "--current-thresholds", str(root / "new.rs"),
                    "--base-waivers", str(root / "old.md"),
                    "--current-waivers", str(root / "new.md"),
                ],
                text=True, capture_output=True,
            )

    def test_unwaived_decrease_is_red(self):
        result = self.run_check([("library", "0.9")], [("library", "0.8")])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("library 0.9 -> 0.8", result.stderr)

    def test_raise_no_change_and_new_screen_are_green(self):
        result = self.run_check(
            [("same", "0.9"), ("raised", "0.8")],
            [("same", "0.9"), ("raised", "0.81"), ("new", "0.1")],
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_new_exact_waiver_allows_decrease(self):
        waiver = "RATCHET-WAIVER: library 0.9 -> 0.8 — legitimate rendering fix\n"
        result = self.run_check(
            [("library", "0.9")], [("library", "0.8")], new_waivers=waiver
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("mockup ratchet waived", result.stdout)

    def test_preexisting_waiver_cannot_authorize_another_decrease(self):
        waiver = "RATCHET-WAIVER: library 0.9 -> 0.8 — old waiver\n"
        result = self.run_check(
            [("library", "0.9")], [("library", "0.8")],
            old_waivers=waiver, new_waivers=waiver,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)

    def test_removed_entry_is_red(self):
        result = self.run_check([("library", "0.9")], [])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("ratchet entry removed: library", result.stderr)

    def test_new_removal_waiver_is_green(self):
        waiver = (
            "RATCHET-WAIVER: library 0.9 -> REMOVED — PR launcher#64: "
            "the route was intentionally retired\n"
        )
        result = self.run_check(
            [("library", "0.9")], [], new_waivers=waiver
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("mockup ratchet removal waived", result.stdout)

    def test_rename_without_removal_waiver_is_red(self):
        result = self.run_check(
            [("library", "0.9")], [("collection", "0.9")]
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("ratchet entry removed: library", result.stderr)

    def test_removal_waiver_without_pr_is_an_error(self):
        waiver = "RATCHET-WAIVER: library 0.9 -> REMOVED — route retired\n"
        result = self.run_check(
            [("library", "0.9")], [], new_waivers=waiver
        )
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("removed-entry waiver must name the PR", result.stderr)


if __name__ == "__main__":
    unittest.main()
