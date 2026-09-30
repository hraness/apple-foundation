import unittest

from verify_crate_registry import verify_registry_archive


class RegistryArchiveTests(unittest.TestCase):
    def test_archive_must_match_and_remain_available(self):
        checksum = "ab" * 32
        valid = {"crate": "apple-foundation", "num": "0.2.0", "checksum": checksum, "yanked": False}

        def verify(published):
            verify_registry_archive({"version": published}, "apple-foundation", "0.2.0", checksum)

        verify(valid)
        for changed in ({"crate": "other"}, {"num": "0.1.0"}, {"checksum": "cd" * 32}, {"yanked": True}):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                verify({**valid, **changed})
        for missing in ("crate", "num", "checksum", "yanked"):
            with self.subTest(missing=missing), self.assertRaises(ValueError):
                verify({key: value for key, value in valid.items() if key != missing})
        with self.assertRaises(ValueError):
            verify_registry_archive({"version": valid}, "apple-foundation", "0.2.0", "invalid")


if __name__ == "__main__":
    unittest.main()
