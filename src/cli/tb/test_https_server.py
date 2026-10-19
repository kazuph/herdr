"""Local TLS download service used only by the Rust test binary."""
import http.server
import ssl
import sys


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path == '/redirect':
            self.send_response(302)
            self.send_header('Location', 'http://localhost:1/forbidden')
            self.end_headers()
        elif self.path == '/missing':
            self.send_error(404)
        else:
            self.send_response(200)
            self.send_header('Content-Length', '100' if self.path == '/partial' else '4')
            self.end_headers()
            self.wfile.write(b'test')
            self.close_connection = True


httpd = http.server.HTTPServer(('127.0.0.1', 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[1], sys.argv[2])
httpd.socket = context.wrap_socket(httpd.socket, server_side=True)
print(httpd.server_port, flush=True)
httpd.serve_forever()
