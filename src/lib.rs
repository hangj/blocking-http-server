#![doc = include_str!("../README.md")]

use std::ops::Deref;
use std::ops::DerefMut;

use bytes::BytesMut;
pub use http::*;
use io::Read;
use io::Write;
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
    request: Request<BytesMut>,
    stream: TcpStream,
}

impl HttpRequest {
    pub fn header_bytes(&self) -> &[u8] {
        &self.header_buf
    }

    pub fn respond<T: AsRef<[u8]>>(
        &self,
        response: impl std::borrow::Borrow<Response<T>>,
    ) -> io::Result<()> {
        let version = self.version();
        let mut stream = &self.stream;

        let response: &Response<T> = response.borrow();
        // let version = response.version();
        let status = response.status();
        let headers = response.headers();
        let body = response.body().as_ref();

        write!(
            stream,
            "{:?} {} {}\r\n",
            version,
            status.as_str(),
            status.canonical_reason().unwrap_or("Unknown"),
        )?;

        // println!("write_response: {}", text);

        // if !headers.contains_key(header::DATE) {
        //     let date = time::strftime("%a, %d %b %Y %H:%M:%S GMT", &time::now_utc()).unwrap();
        //     write!(stream, "date: {}\r\n", date)?;
        // }
        if !headers.contains_key(header::CONNECTION) {
            write!(stream, "connection: close\r\n")?;
        }
        if !headers.contains_key(header::CONTENT_LENGTH) {
            write!(stream, "content-length: {}\r\n", body.len())?;
        }
        for (k, v) in headers.iter() {
            write!(
                stream,
                "{}: {}\r\n",
                k.as_str(),
                v.to_str().unwrap_or("unknown")
            )?;
        }

        stream.write_all(b"\r\n")?;
        stream.write_all(body)?;
        stream.flush()?;

        Ok(())
    }
}

impl Deref for HttpRequest {
    type Target = Request<BytesMut>;
    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

impl DerefMut for HttpRequest {
    fn deref_mut(&mut self) -> &mut Self::Target {
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
                // This will not cause reallocation, because the `split_off`ed header_buf and body_buf are dropped at this point.
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

                    let mut content_len = 0;
                    for header in req.headers {
                        builder = builder.header(header.name, header.value);
                        if header.name.eq_ignore_ascii_case("host") {
                            let host = header.value;
                            uri = uri.authority(host);
                        }

                        if header.name.eq_ignore_ascii_case(header::CONTENT_LENGTH.as_str()) {
                            content_len = std::str::from_utf8(header.value).unwrap_or("0").parse::<usize>().unwrap_or(0);
                            if content_len > header_buf.capacity() - offset {
                                return Some(Err(io::Error::new(
                                    io::ErrorKind::Other,
                                    "body too large",
                                )));
                            }
                        }
                    }

                    let mut body_buf = header_buf.split_off(offset);
                    if body_buf.capacity() < content_len {
                        return Some(Err(io::Error::new(io::ErrorKind::Other, "body too large")));
                    }

                    if body_buf.len() >= content_len {
                        body_buf.truncate(content_len);
                    } else {
                        let size = content_len - body_buf.len();
    
                        let mut tmp = body_buf.split_off(body_buf.len());
                        unsafe { tmp.set_len(size) };
    
                        if let Err(e) = stream.read_exact(&mut tmp) {
                            return Some(Err(e));
                        }
                        body_buf.unsplit(tmp);
                    }

                    builder = builder.uri(uri.build().unwrap_or_default());

                    let request = match builder.body(body_buf) {
                        Ok(req) => req,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::Other, e))),
                    };

                    return Some(Ok(HttpRequest {
                        peer_addr: addr,
                        header_buf,
                        request,
                        stream,
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


