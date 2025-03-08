# A simple Blocking HTTP server library

No async, No runtime, just a dead simple blocking http server

# Usage example:

```rust, no_run
use blocking_http_server::*;

fn main() -> anyhow::Result<()> {
    let mut server = Server::bind("127.0.0.1:8000").unwrap();

    for req in server.incoming() {
        let mut req = match req {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Error: {}", e);
                continue;
            }
        };

        match (req.method(), req.uri().path()) {
            (&Method::GET, "/") => {
                let _ = req.response(Response::new("hello world".as_bytes()));
            }
            _ => {
                let _ = req.response(Response::new("404 not found".as_bytes()));
            }
        }
    }
    Ok(())
}
```
