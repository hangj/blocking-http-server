# A simple Blocking HTTP server library

No async, No runtime, just a dead simple blocking http server

# Usage example:

```rust, no_run
use blocking_http_server::*;

fn main() -> anyhow::Result<()> {
    let mut server = Server::bind("127.0.0.1:8000")?;

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
                let _ = req.respond(Response::new("hello world"));
            }
            _ => {
                let _ = req.respond(
                    Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("404 Not Found")
                        .unwrap(),
                );
             }
        }
    }
    Ok(())
}
```
