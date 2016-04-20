use std::net::TcpListener;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::fmt;
use std::io::Cursor;
use std::io::Read;
use std::convert::From;
use std::sync::Arc;
use std::error::Error;

use solicit::server::SimpleServer;
use solicit::http::server::StreamFactory;
use solicit::http::server::ServerConnection;
use solicit::http::server::ServerSession;
use solicit::http::HttpScheme;
use solicit::http::StreamId;
use solicit::http::Header;
use solicit::http::HeaderPart;
use solicit::http::OwnedHeader;
use solicit::http::HttpResult;
use solicit::http::Request;
use solicit::http::Response;
use solicit::http::StaticResponse;
use solicit::http::priority::SimplePrioritizer;
use solicit::http::connection::HttpConnection;
use solicit::http::connection::EndStream;
use solicit::http::connection::SendStatus;
use solicit::http::connection::SendFrame;
use solicit::http::connection::DataChunk;
use solicit::http::session::SessionState;
use solicit::http::session::DefaultSessionState;
use solicit::http::session::DefaultStream;
use solicit::http::session::Stream;
use solicit::http::session::Server as ServerMarker;
use solicit::http::session::StreamState;
use solicit::http::session::StreamDataChunk;
use solicit::http::session::StreamDataError;
use solicit::http::transport::TransportStream;
use solicit::http::transport::TransportReceiveFrame;

use openssl::ssl::{Ssl, SslContext, SslStream, SslMethod, SSL_VERIFY_NONE};

use grpc;
use errors::GrpcResult;
use method::ServerServiceDefinition;

struct BsDebug<'a>(&'a [u8]);

impl<'a> fmt::Debug for BsDebug<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        try!(write!(fmt, "b\""));
        let u8a: &[u8] = self.0;
        for &c in u8a {
            // ASCII printable
            if c >= 0x20 && c < 0x7f {
                try!(write!(fmt, "{}", c as char));
            } else {
                try!(write!(fmt, "\\x{:02x}", c));
            }
        }
        try!(write!(fmt, "\""));
        Ok(())
    }
}

struct HeaderDebug<'a>(&'a Header<'a, 'a>);

impl<'a> fmt::Debug for HeaderDebug<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(fmt,
               "Header {{ name: {:?}, value: {:?} }}",
               BsDebug(self.0.name()),
               BsDebug(self.0.value()))
    }
}

/// The struct represents a fully received request.
pub struct GrpcRequest<'a, 'n, 'v>
    where 'n: 'a,
          'v: 'a
{
    pub stream_id: StreamId,
    pub headers: &'a [Header<'n, 'v>],
    pub body: &'a [u8],
}

pub type GrpcResponse = StaticResponse;

#[derive(Clone, Default)]
pub struct GrpcRouter {
}

unsafe impl Send for GrpcRouter {}
unsafe impl Sync for GrpcRouter {}

impl GrpcRouter {
    pub fn new() -> GrpcRouter {
        Default::default()
    }

    fn handle_request(&self, req: GrpcRequest) -> GrpcResponse {
        Response {
            headers: vec![
                        Header::new(b":status", b"200"),
                        Header::new(&b"content-type"[..], &b"application/grpc"[..]),
                    ],
            body: req.body.to_vec(),
            stream_id: req.stream_id,
        }
    }
}

type GrpcStream = DefaultStream;

struct GrpcStreamFactory;

impl StreamFactory for GrpcStreamFactory {
    type Stream = GrpcStream;

    fn create(&mut self, id: StreamId) -> GrpcStream {
        GrpcStream::with_id(id)
    }
}

struct GrpcServerConnection<TS: TransportStream> {
    conn: ServerConnection<GrpcStreamFactory, DefaultSessionState<ServerMarker, GrpcStream>>,
    receiver: TS,
    sender: TS,
    router: Arc<GrpcRouter>,
}

impl<TS: TransportStream + Sized> GrpcServerConnection<TS> {
    fn new(router: Arc<GrpcRouter>, mut stream: TS) -> GrpcResult<Self> {
        let mut preface = [0; 24];

        try!(TransportStream::read_exact(&mut stream, &mut preface));

        if &preface != b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n" {
            panic!();
        }

        let conn = HttpConnection::new(HttpScheme::Http);
        let state = DefaultSessionState::<ServerMarker, _>::new();
        let conn = ServerConnection::with_connection(conn, state, GrpcStreamFactory);
        let mut server = GrpcServerConnection {
            conn: conn,
            receiver: try!(stream.try_split()),
            sender: stream,
            router: router,
        };

        // Initialize the connection -- send own settings and process the peer's
        try!(server.conn.send_settings(&mut server.sender));
        try!(server.conn.expect_settings(&mut TransportReceiveFrame::new(&mut server.receiver),
                                         &mut server.sender));

        Ok(server)
    }

    fn handle_requests(&mut self) -> HttpResult<Vec<GrpcResponse>> {
        let router = self.router.clone();
        let closed = self.conn
                         .state
                         .iter()
                         .filter(|&(_, ref s)| s.is_closed_remote());

        let responses = closed.map(|(&stream_id, stream)| {
            let req = GrpcRequest {
                stream_id: stream_id,
                headers: stream.headers.as_ref().unwrap(),
                body: &stream.body,
            };

            router.handle_request(req)
        });

        Ok(responses.collect())
    }

    fn prepare_responses(&mut self, responses: Vec<GrpcResponse>) -> HttpResult<()> {
        for response in responses {
            try!(self.conn.start_response(response.headers,
                                          response.stream_id,
                                          EndStream::No,
                                          &mut self.sender));
            let mut stream = self.conn.state.get_stream_mut(response.stream_id).unwrap();
            stream.set_full_data(response.body);
        }

        Ok(())
    }

    fn flush_streams(&mut self) -> HttpResult<()> {
        while let SendStatus::Sent = try!(self.conn.send_next_data(&mut self.sender)) {}

        Ok(())
    }

    fn reap_streams(&mut self) -> HttpResult<()> {
        // Moves the streams out of the state and then drops them
        let closed = self.conn.state.get_closed();

        debug!("stream closed: {:?}",
               closed.iter().map(|s| s.stream_id).collect::<Vec<_>>());

        Ok(())
    }

    fn handle_next(&mut self) -> HttpResult<()> {
        try!(self.conn.handle_next_frame(&mut TransportReceiveFrame::new(&mut self.receiver),
                                         &mut self.sender));

        let responses = try!(self.handle_requests());

        try!(self.prepare_responses(responses));
        try!(self.flush_streams());
        try!(self.reap_streams());

        Ok(())
    }

    pub fn run(&mut self) {
        while let Err(ref err) = self.handle_next() {
            warn!("handle request failed, {}", err);

            break;
        }
    }
}

pub trait GrpcServer<TS: TransportStream>: Sized {
    fn new<A: ToSocketAddrs>(router: Arc<GrpcRouter>, addr: A) -> GrpcResult<Self>;

    fn accept(&self) -> GrpcResult<TS>;

    fn router(&self) -> Arc<GrpcRouter>;

    fn run<F: FnMut(GrpcServerConnection<TS>)>(&mut self, mut handler: F) {
        loop {
            match self.accept() {
                Ok(stream) => {
                    match GrpcServerConnection::<TS>::new(self.router(), stream) {
                        Ok(conn) => handler(conn),
                        Err(ref err) => warn!("create connection error, {}", err),
                    }
                }
                Err(ref err) => warn!("accept connection error, {}", err),
            }
        }
    }
}

pub struct TcpServer {
    router: Arc<GrpcRouter>,
    listener: TcpListener,
}

impl GrpcServer<TcpStream> for TcpServer {
    fn new<A: ToSocketAddrs>(router: Arc<GrpcRouter>, addr: A) -> GrpcResult<TcpServer> {
        let listener = try!(TcpListener::bind(addr));

        info!("TCP server is listening on {}", try!(listener.local_addr()));

        Ok(TcpServer {
            router: router,
            listener: listener,
        })
    }

    fn router(&self) -> Arc<GrpcRouter> {
        self.router.clone()
    }

    fn accept(&self) -> GrpcResult<TcpStream> {
        let (stream, addr) = try!(self.listener.accept());

        info!("accepted TCP connection from {}", addr);

        Ok(stream)
    }
}

pub struct SslServer {
    router: Arc<GrpcRouter>,
    listener: TcpListener,
    context: Arc<SslContext>,
}

impl GrpcServer<SslStream<TcpStream>> for SslServer {
    fn new<A: ToSocketAddrs>(router: Arc<GrpcRouter>, addr: A) -> GrpcResult<SslServer> {
        let listener = try!(TcpListener::bind(addr));

        info!("SSL server is listening on {}", try!(listener.local_addr()));

        Ok(SslServer {
            router: router,
            listener: listener,
            context: Arc::new({
                let mut ctxt = try!(SslContext::new(SslMethod::Tlsv1_1));

                try!(ctxt.set_cipher_list("DEFAULT"));
                ctxt.set_verify(SSL_VERIFY_NONE, None);

                // TODO more SSL config

                ctxt
            }),
        })
    }

    fn router(&self) -> Arc<GrpcRouter> {
        self.router.clone()
    }

    fn accept(&self) -> GrpcResult<SslStream<TcpStream>> {
        let (stream, addr) = try!(self.listener.accept());

        debug!("SSL handshake from {}", addr);

        let stream = try!(SslStream::accept(&*self.context, stream));

        info!("accepted SSL connection from {}", addr);

        Ok(stream)
    }
}
