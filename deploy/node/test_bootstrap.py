import hashlib
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from bootstrap import configure


class BootstrapTest(unittest.TestCase):
    def test_restart_preserves_login_preferences_and_pairing(self):
        with tempfile.TemporaryDirectory() as tmp:
            host, creds = Path(tmp) / 'host.toml', Path(tmp) / 'web.json'
            configure(host, creds, {})
            first = tomllib.loads(host.read_text())
            host.write_text(host.read_text().replace('power_allowed = true', 'power_allowed = false'))
            configure(host, creds, {})
            second = tomllib.loads(host.read_text())
            self.assertEqual(first['sunshine_pass'], second['sunshine_pass'])
            self.assertFalse(second['power_allowed'])
            web = json.loads(creds.read_text())
            self.assertEqual(web['password'], hashlib.sha256((second['sunshine_pass'] + web['salt']).encode()).digest()[::-1].hex().upper())
            self.assertEqual(os.stat(host).st_mode & 0o777, 0o600)

    def test_explicit_password_and_quotes_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            host, creds = Path(tmp) / 'host.toml', Path(tmp) / 'web.json'
            password = 'a"b\\c\n$123'
            configure(host, creds, {'BROLINK_PASS': password})
            self.assertEqual(tomllib.loads(host.read_text())['sunshine_pass'], password)
            configure(host, creds, {'BROLINK_PASS': 'new'})
            self.assertEqual(tomllib.loads(host.read_text())['sunshine_pass'], 'new')

    def test_corruption_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as tmp:
            host, creds = Path(tmp) / 'host.toml', Path(tmp) / 'web.json'
            host.write_text('bad = [')
            with self.assertRaises(tomllib.TOMLDecodeError):
                configure(host, creds, {})
            self.assertEqual(host.read_text(), 'bad = [')
            self.assertFalse(creds.exists())


if __name__ == '__main__':
    unittest.main()
