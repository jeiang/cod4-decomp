"""Browser page transport checks; run with `python3 -m unittest web.test_serve`."""

import http.client
import http.server
import pathlib
import threading
import unittest

import serve


class ControlBridgeTests(unittest.TestCase):
    def test_post_without_a_control_page_returns_504(self):
        root = pathlib.Path(serve.__file__).parent
        handler = lambda *args, **kwargs: serve.Handler(*args, directory=str(root), **kwargs)
        with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            old_timeout = serve.CONTROL_REPLY_TIMEOUT
            serve.CONTROL_REPLY_TIMEOUT = 0.01
            try:
                connection = http.client.HTTPConnection(*server.server_address, timeout=1)
                connection.request("POST", "/control", '{"id":1,"cmd":"state"}')
                response = connection.getresponse()
                self.assertEqual(response.status, 504)
                self.assertIn(b"is it open with ?control?", response.read())
                connection.close()
            finally:
                serve.CONTROL_REPLY_TIMEOUT = old_timeout
                server.shutdown()
                thread.join()


if __name__ == "__main__":
    unittest.main()
