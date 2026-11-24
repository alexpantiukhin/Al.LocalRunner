import unittest

from release_notes import breaking_line, notes, protocol_major

SOURCE = "pub const PROTOCOL_VERSION: Version = Version { major: 2, minor: 0 };"


class ReleaseNotes(unittest.TestCase):
    def test_reads_the_major_of_the_protocol(self):
        self.assertEqual(protocol_major(SOURCE), 2)
        self.assertEqual(protocol_major("nothing here"), None)

    def test_a_changed_major_gives_a_breaking_line(self):
        line = breaking_line(1, 2, "v0.5.0")
        self.assertIn("Breaking", line)
        self.assertIn("from 1 to 2", line)
        self.assertIn("broker update-service", line)

    def test_the_same_or_an_unknown_major_gives_none(self):
        self.assertIsNone(breaking_line(2, 2, "v0.8.0"))
        self.assertIsNone(breaking_line(None, 2, ""))
        self.assertIsNone(breaking_line(2, None, "v0.8.0"))

    def test_the_pending_notes_follow_the_breaking_line(self):
        text = notes(1, 2, "v0.5.0", "Deprecated: x\n")
        self.assertTrue(text.startswith("**Breaking:**"))
        self.assertTrue(text.endswith("Deprecated: x"))
        self.assertEqual(notes(2, 2, "v0.8.0", ""), "")


if __name__ == "__main__":
    unittest.main()
