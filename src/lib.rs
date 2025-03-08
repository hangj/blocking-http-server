#![doc = include_str!("../README.md")]

use std::ops::Deref;
use std::ops::DerefMut;

use bytes::BytesMut;
pub use http::*;
use io::Read;
use io::Write as _;
use std::fmt::Write as _;
use std::io;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::net::ToSocketAddrs;

pub struct Server {
    listener: TcpListener,
    req_size_limit: usize,

    buf: BytesMut,
}

impl Server {
    const DEFAULT_REQ_SIZE_LIMIT: usize = 4096;
    const HEADER_COUNT_LIMIT: usize = 64;

    pub fn bind(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        Ok(Self {
            listener,
            req_size_limit: Self::DEFAULT_REQ_SIZE_LIMIT,
            buf: BytesMut::with_capacity(Self::DEFAULT_REQ_SIZE_LIMIT),
        })
    }

    pub fn set_request_size_limit(&mut self, limit: usize) {
        self.buf = BytesMut::with_capacity(limit);
        self.req_size_limit = limit;
    }

    pub fn incoming(&mut self) -> Incoming {
        Incoming { server: self }
    }

    pub fn recv(&mut self) -> io::Result<HttpRequest> {
        self.incoming().next().unwrap()
    }
}

#[derive(Debug)]
pub struct HttpRequest {
    pub peer_addr: SocketAddr,

    header_buf: BytesMut,
    body_buf: BytesMut,
    request: Request<TcpStream>,

    responsed: bool,
}

impl Drop for HttpRequest {
    fn drop(&mut self) {
        if !self.responsed {
            let _ = self.response(
                &Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body("404 Not Found".as_bytes())
                    .unwrap(),
            );
        }
    }
}

impl HttpRequest {
    pub fn header_bytes(&self) -> &[u8] {
        &self.header_buf
    }
    pub fn body(&mut self) -> io::Result<&[u8]> {
        let content_len = self
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|len| len.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok());

        match content_len {
            Some(len) => {
                if self.body_buf.len() >= len {
                    self.body_buf.truncate(len);
                } else {
                    let size = len - self.body_buf.len();

                    let mut tmp = self.body_buf.split_off(self.body_buf.len());
                    if tmp.capacity() < size {
                        return Err(io::Error::new(io::ErrorKind::Other, "body too large"));
                    }
                    unsafe { tmp.set_len(size) };

                    let stream = self.deref_mut().body_mut();

                    stream.read_exact(&mut tmp)?;
                    self.body_buf.unsplit(tmp);
                }
            }
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "missing content-length",
                ))
            }
        }

        Ok(&self.body_buf)
    }

    pub fn response<T: std::borrow::Borrow<[u8]>>(
        &mut self,
        response: &Response<T>,
    ) -> io::Result<()> {
        self.responsed = true;

        let version = self.version();
        let stream = self.deref_mut().body_mut();

        // let version = response.version();
        let status = response.status();
        let headers = response.headers();
        let body: &[u8] = response.body().borrow();

        let mut text = format!(
            "{:?} {} {}\r\n",
            version,
            status.as_str(),
            status.canonical_reason().unwrap_or("Unknown"),
        );

        // println!("write_response: {}", text);

        // if !headers.contains_key(header::DATE) {
        //     let date = time::strftime("%a, %d %b %Y %H:%M:%S GMT", &time::now_utc()).unwrap();
        //     write!(text, "date: {}\r\n", date).unwrap();
        // }
        if !headers.contains_key(header::CONNECTION) {
            write!(text, "connection: close\r\n")
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        if !headers.contains_key(header::CONTENT_LENGTH) {
            write!(text, "content-length: {}\r\n", body.len())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        for (k, v) in headers.iter() {
            write!(
                text,
                "{}: {}\r\n",
                k.as_str(),
                v.to_str().unwrap_or("unknown")
            )
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        stream.write_all(text.as_bytes())?;
        stream.write_all(b"\r\n")?;
        stream.write_all(body)?;
        stream.flush()?;

        Ok(())
    }
}

impl Deref for HttpRequest {
    type Target = Request<TcpStream>;
    fn deref(&self) -> &Request<TcpStream> {
        &self.request
    }
}

impl DerefMut for HttpRequest {
    fn deref_mut(&mut self) -> &mut Request<TcpStream> {
        &mut self.request
    }
}

pub struct Incoming<'a> {
    server: &'a mut Server,
}

impl Iterator for Incoming<'_> {
    type Item = io::Result<HttpRequest>;
    fn next(&mut self) -> Option<Self::Item> {
        let (mut stream, addr) = match self.server.listener.accept() {
            Ok((stream, addr)) => {
                let _ = stream.set_nodelay(true);
                (stream, addr)
            }
            Err(e) => return Some(Err(e)),
        };

        {
            // prepare the buffer
            let buf = &mut self.server.buf;
            buf.clear();
            if self.server.req_size_limit > buf.capacity() {
                // This will not cause reallocation, because the `split_off`ed body_buf is dropped at this point.
                buf.reserve(self.server.req_size_limit - buf.capacity());
            }
        }

        let mut header_buf = self.server.buf.split_off(0);

        loop {
            let mut tmp = header_buf.split_off(header_buf.len());
            unsafe { tmp.set_len(tmp.capacity()) };

            match stream.read(&mut tmp) {
                Ok(0) => {
                    tmp.clear();
                    header_buf.unsplit(tmp);
                    return Some(Err(io::Error::new(
                        io::ErrorKind::Other,
                        "uncomplete request header",
                    )));
                }
                Ok(n) => {
                    unsafe { tmp.set_len(n) };
                    header_buf.unsplit(tmp);

                    let mut headers = [httparse::EMPTY_HEADER; Server::HEADER_COUNT_LIMIT];
                    let mut req = httparse::Request::new(&mut headers);

                    let offset = match req.parse(&header_buf) {
                        Ok(httparse::Status::Complete(offset)) => offset,
                        Ok(httparse::Status::Partial) => continue,
                        Err(e) => {
                            // eprintln!("error: {e}");
                            return Some(Err(io::Error::new(io::ErrorKind::Other, e)));
                        }
                    };

                    let version = match req.version {
                        Some(0) => Version::HTTP_10,
                        Some(1) => Version::HTTP_11,
                        Some(_) => Version::HTTP_11,
                        None => Version::HTTP_11,
                    };

                    let mut uri = Uri::builder()
                        .scheme(uri::Scheme::HTTP)
                        .path_and_query(req.path.unwrap_or("/"));

                    let mut builder = Request::builder()
                        .method(req.method.unwrap_or("GET"))
                        .version(version);

                    for header in req.headers {
                        builder = builder.header(header.name, header.value);
                        if header.name.eq_ignore_ascii_case("host") {
                            let host = header.value;
                            uri = uri.authority(host);
                        }
                    }

                    builder = builder.uri(uri.build().unwrap_or_default());

                    let request = match builder.body(stream) {
                        Ok(req) => req,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::Other, e))),
                    };

                    let body_buf = header_buf.split_off(offset);

                    return Some(Ok(HttpRequest {
                        peer_addr: addr,
                        header_buf,
                        body_buf,
                        request,
                        responsed: false,
                    }));
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::Interrupted
                        || e.kind() == io::ErrorKind::WouldBlock
                    {
                        tmp.clear();
                        header_buf.unsplit(tmp);
                        continue;
                    }
                    // eprintln!("error: {e}");
                    return Some(Err(e));
                }
            };
        }
    }
}
