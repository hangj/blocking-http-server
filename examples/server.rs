use blocking_http_server::*;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!(
            "Usage: {} <addr>\n\nExample: {} 127.0.0.1:8080",
            args[0], args[0]
        );
        std::process::exit(1);
    }
    let mut server = Server::bind(&args[1])?;

    for req in server.incoming() {
        let mut req = match req {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Error: {}", e);
                continue;
            }
        };

        println!("{} {} {}", req.peer_addr, req.method(), req.uri().path());

        match (req.method(), req.uri().path()) {
            (&Method::GET, "/") => {
                let _ = req.response(&Response::new("index".as_bytes()));
            }
            (&Method::GET, "/hello") => {
                let _ = req.response(&Response::new("hello world".as_bytes()));
            }
            _ => {
                let _ = req.response(
                    &Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("404 Not Found".as_bytes())
                        .unwrap(),
                );
            }
        }
    }
    Ok(())
}
