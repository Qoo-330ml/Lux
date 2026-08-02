import importlib.util
import json
import pathlib
import sys
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("probe.py")
SPEC = importlib.util.spec_from_file_location("lux_compat_probe", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = probe
SPEC.loader.exec_module(probe)


class ProbeFormattingTests(unittest.TestCase):
    def test_login_record_omits_access_token_and_nested_sensitive_values(self):
        event = probe.record_event(
            "POST",
            "/Users/AuthenticateByName",
            200,
            {
                "AccessToken": "do-not-write-this",
                "ServerId": "server-1",
                "User": {"Name": "probe", "Id": "user-1"},
            },
        )

        serialized = json.dumps(event, ensure_ascii=False)
        self.assertNotIn("do-not-write-this", serialized)
        self.assertNotIn("user-1", serialized)
        self.assertNotIn("probe", serialized)
        self.assertEqual(event["response"]["fields"], ["ServerId", "User"])

    def test_error_record_contains_only_stable_shape(self):
        event = probe.record_event(
            "GET",
            "/System/Info",
            401,
            {"Message": "token=do-not-write-this", "Response": "secret"},
        )

        self.assertEqual(event, {
            "method": "GET",
            "path": "/System/Info",
            "status": 401,
            "response": {"fields": ["Message", "Response"]},
        })

    def test_public_user_list_records_count_without_user_values(self):
        event = probe.record_event(
            "GET",
            "/Users/Public",
            200,
            [{"Id": "user-1", "Name": "probe", "Policy": {"IsAdministrator": True}}],
        )

        self.assertEqual(event["response"], {"type": "list", "count": 1})


if __name__ == "__main__":
    unittest.main()
