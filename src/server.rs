use std::net::TcpListener;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::fmt;
use std::str;
use std::convert::From;
use std::sync::Arc;
use std::time::Duration;
use std::error::Error;

use solicit::http::server::StreamFactory;
use solicit::http::server::ServerConnection;
use solicit::http::HttpScheme;
use solicit::http::StreamId;
use solicit::http::Header;
use solicit::http::HttpResult;
use solicit::http::Response;
use solicit::http::StaticResponse;
use solicit::http::connection::HttpConnection;
use solicit::http::connection::EndStream;
use solicit::http::connection::SendStatus;
use solicit::http::session::SessionState;
use solicit::http::session::DefaultSessionState;
use solicit::http::session::DefaultStream;
use solicit::http::session::Stream;
use solicit::http::session::Server as ServerMarker;
use solicit::http::transport::TransportStream;
use solicit::http::transport::TransportReceiveFrame;

use openssl::ssl::{SslContext, SslStream, SslMethod, SSL_VERIFY_NONE};

use errors::{GrpcError, GrpcResult};

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

    scheme: Option<&'a str>,
    method: Option<&'a str>,
    path: Option<&'a str>,
    authority: Option<&'a str>,
    timeout: Option<Duration>,
    content_type: Option<&'a str>,
    encoding: Option<&'a str>,
    accept_encoding: Option<&'a str>,
    user_agent: Option<&'a str>,
    message_type: Option<&'a str>,
}

impl<'a, 'n, 'v> GrpcRequest<'a, 'n, 'v> {
    fn new(stream: &'a GrpcStream) -> GrpcResult<GrpcRequest<'a, 'n, 'v>> {
        if stream.stream_id.is_none() || stream.headers.is_none() ||
           stream.headers.as_ref().unwrap().len() == 0 {
            return Err(GrpcError::BadRequest("malformed stream"));
        }

        let mut req = GrpcRequest {
            stream_id: stream.stream_id.unwrap(),
            headers: stream.headers.as_ref().unwrap(),
            body: &stream.body,
            scheme: None,
            method: None,
            path: None,
            authority: None,
            timeout: None,
            content_type: None,
            encoding: None,
            accept_encoding: None,
            user_agent: None,
            message_type: None,
        };

        try!(req.parse_headers());

        Ok(req)
    }

    fn parse_headers(&mut self) -> GrpcResult<()> {
        unsafe {
            for header in self.headers {
                match header.name() {
                    b":scheme" => {
                        self.scheme = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b":method" => {
                        self.method = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b":path" => {
                        self.path = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b":authority" => {
                        self.authority = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b"grpc-timeout" => {
                        match Self::parse_timeout(header.value()) {
                            Ok(d) => {
                                self.timeout = Some(d);
                            }
                            Err(err) => {
                                warn!("parse grpc timeout error, {}", err);

                            }
                        }
                    }
                    b"content-type" => {
                        self.content_type = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b"grpc-encoding" => {
                        self.encoding = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b"grpc-accept-encoding" => {
                        self.accept_encoding = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b"user-agent" => {
                        self.user_agent = Some(str::from_utf8_unchecked(header.value()));
                    }
                    b"grpc-message-type" => {
                        self.message_type = Some(str::from_utf8_unchecked(header.value()));
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn parse_timeout(s: &[u8]) -> GrpcResult<Duration> {
        let (v, u) = s.split_at(s.len() - 1);
        let value: u64 = try!(unsafe { str::from_utf8_unchecked(v) }.parse());
        let (secs, nanos) = match u {
            b"H" => (value * 60 * 60, 0),
            b"M" => (value * 60, 0),
            b"S" => (value, 0),
            b"m" => (value / 1_000, (value % 1_000) * 1_000_000),
            b"u" => (value / 1_000_000, (value % 1_000_000) * 1_000),
            b"n" => (value / 1_000_000_000, value % 1_000_000_000),
            _ => (value, 0),
        };

        Ok(Duration::new(secs, nanos as u32))
    }
}

pub enum GrpcStatus {
    /// OK is returned on success.
    Ok = 0,

    /// Canceled indicates the operation was cancelled (typically by the caller).
    Canceled = 1,

    /// Unknown error.  An example of where this error may be returned is
    /// if a Status value received from another address space belongs to
    /// an error-space that is not known in this address space.  Also
    /// errors raised by APIs that do not return enough error information
    /// may be converted to this error.
    Unknown = 2,

    /// InvalidArgument indicates client specified an invalid argument.
    /// Note that this differs from FailedPrecondition. It indicates arguments
    /// that are problematic regardless of the state of the system
    /// (e.g., a malformed file name).
    InvalidArgument = 3,

    /// DeadlineExceeded means operation expired before completion.
    /// For operations that change the state of the system, this error may be
    /// returned even if the operation has completed successfully. For
    /// example, a successful response from a server could have been delayed
    /// long enough for the deadline to expire.
    DeadlineExceeded = 4,

    /// NotFound means some requested entity (e.g., file or directory) was
    /// not found.
    NotFound = 5,

    /// AlreadyExists means an attempt to create an entity failed because one
    /// already exists.
    AlreadyExists = 6,

    /// PermissionDenied indicates the caller does not have permission to
    /// execute the specified operation. It must not be used for rejections
    /// caused by exhausting some resource (use ResourceExhausted
    /// instead for those errors).  It must not be
    /// used if the caller cannot be identified (use Unauthenticated
    /// instead for those errors).
    PermissionDenied = 7,

    /// Unauthenticated indicates the request does not have valid
    /// authentication credentials for the operation.
    Unauthenticated = 16,

    /// ResourceExhausted indicates some resource has been exhausted, perhaps
    /// a per-user quota, or perhaps the entire file system is out of space.
    ResourceExhausted = 8,

    /// FailedPrecondition indicates operation was rejected because the
    /// system is not in a state required for the operation's execution.
    /// For example, directory to be deleted may be non-empty, an rmdir
    /// operation is applied to a non-directory, etc.
    ///
    /// A litmus test that may help a service implementor in deciding
    /// between FailedPrecondition, Aborted, and Unavailable:
    ///  (a) Use Unavailable if the client can retry just the failing call.
    ///  (b) Use Aborted if the client should retry at a higher-level
    ///      (e.g., restarting a read-modify-write sequence).
    ///  (c) Use FailedPrecondition if the client should not retry until
    ///      the system state has been explicitly fixed.  E.g., if an "rmdir"
    ///      fails because the directory is non-empty, FailedPrecondition
    ///      should be returned since the client should not retry unless
    ///      they have first fixed up the directory by deleting files from it.
    ///  (d) Use FailedPrecondition if the client performs conditional
    ///      REST Get/Update/Delete on a resource and the resource on the
    ///      server does not match the condition. E.g., conflicting
    ///      read-modify-write on the same resource.
    FailedPrecondition = 9,

    /// Aborted indicates the operation was aborted, typically due to a
    /// concurrency issue like sequencer check failures, transaction aborts,
    /// etc.
    ///
    /// See litmus test above for deciding between FailedPrecondition,
    /// Aborted, and Unavailable.
    Aborted = 10,

    /// OutOfRange means operation was attempted past the valid range.
    /// E.g., seeking or reading past end of file.
    ///
    /// Unlike InvalidArgument, this error indicates a problem that may
    /// be fixed if the system state changes. For example, a 32-bit file
    /// system will generate InvalidArgument if asked to read at an
    /// offset that is not in the range [0,2^32-1], but it will generate
    /// OutOfRange if asked to read from an offset past the current
    /// file size.
    ///
    /// There is a fair bit of overlap between FailedPrecondition and
    /// OutOfRange.  We recommend using OutOfRange (the more specific
    /// error) when it applies so that callers who are iterating through
    /// a space can easily look for an OutOfRange error to detect when
    /// they are done.
    OutOfRange = 11,

    /// Unimplemented indicates operation is not implemented or not
    /// supported/enabled in this service.
    Unimplemented = 12,

    /// Internal errors.  Means some invariants expected by underlying
    /// system has been broken.  If you see one of these errors,
    /// something is very broken.
    Internal = 13,

    /// Unavailable indicates the service is currently unavailable.
    /// This is a most likely a transient condition and may be corrected
    /// by retrying with a backoff.
    ///
    /// See litmus test above for deciding between FailedPrecondition,
    /// Aborted, and Unavailable.
    Unavailable = 14,

    /// DataLoss indicates unrecoverable data loss or corruption.
    DataLoss = 15,
}

impl GrpcStatus {
    fn as_str(&self) -> &'static [u8] {
        match self {
            &GrpcStatus::Ok => b"0",
            &GrpcStatus::Canceled => b"1",
            &GrpcStatus::Unknown => b"2",
            &GrpcStatus::InvalidArgument => b"3",
            &GrpcStatus::DeadlineExceeded => b"4",
            &GrpcStatus::NotFound => b"5",
            &GrpcStatus::AlreadyExists => b"6",
            &GrpcStatus::PermissionDenied => b"7",
            &GrpcStatus::ResourceExhausted => b"8",
            &GrpcStatus::FailedPrecondition => b"9",
            &GrpcStatus::Aborted => b"10",
            &GrpcStatus::OutOfRange => b"11",
            &GrpcStatus::Unimplemented => b"12",
            &GrpcStatus::Internal => b"13",
            &GrpcStatus::Unavailable => b"14",
            &GrpcStatus::DataLoss => b"15",
            &GrpcStatus::Unauthenticated => b"16",
        }
    }

    fn reason(&self) -> &'static str {
        match self {
            &GrpcStatus::Ok => "Ok",
            &GrpcStatus::Canceled => "Canceled",
            &GrpcStatus::Unknown => "Unknown",
            &GrpcStatus::InvalidArgument => "InvalidArgument",
            &GrpcStatus::DeadlineExceeded => "DeadlineExceeded",
            &GrpcStatus::NotFound => "NotFound",
            &GrpcStatus::AlreadyExists => "AlreadyExists",
            &GrpcStatus::PermissionDenied => "PermissionDenied",
            &GrpcStatus::ResourceExhausted => "ResourceExhausted",
            &GrpcStatus::FailedPrecondition => "FailedPrecondition",
            &GrpcStatus::Aborted => "Aborted",
            &GrpcStatus::OutOfRange => "OutOfRange",
            &GrpcStatus::Unimplemented => "Unimplemented",
            &GrpcStatus::Internal => "Internal",
            &GrpcStatus::Unavailable => "Unavailable",
            &GrpcStatus::DataLoss => "DataLoss",
            &GrpcStatus::Unauthenticated => "Unauthenticated",
        }
    }
}

pub type GrpcResponse = StaticResponse;

pub enum GrpcResponses {}

impl GrpcResponses {
    fn bad_request(stream_id: StreamId, status: GrpcStatus, msg: &str) -> GrpcResponse {
        Response {
            stream_id: stream_id,
            headers: vec![
                Header::new(b":status", b"400"),
                Header::new(&b"content-type"[..], &b"application/grpc"[..]),
                Header::new(&b"grpc-status"[..], status.as_str()),
                Header::new(&b"grpc-message"[..], Vec::from(msg.as_bytes())),
            ],
            body: Default::default(),
        }
    }
}


#[derive(Clone, Default)]
pub struct GrpcRouter {

}

unsafe impl Send for GrpcRouter {}
unsafe impl Sync for GrpcRouter {}

impl GrpcRouter {
    fn handle_request(&self, req: GrpcRequest) -> GrpcResponse {
        if req.method != Some("POST") {
            return GrpcResponses::bad_request(req.stream_id,
                                              GrpcStatus::Unimplemented,
                                              "only support POST method");
        }

        Response {
            stream_id: req.stream_id,
            headers: vec![
                Header::new(b":status", b"200"),
                Header::new(&b"content-type"[..], &b"application/grpc"[..]),
                Header::new(&b"grpc-status"[..], GrpcStatus::Ok.as_str()),
            ],
            body: req.body.to_vec(),
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

pub struct GrpcServerConnection<TS: TransportStream> {
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
            GrpcRequest::new(stream)
                .map(|request| router.handle_request(request))
                .unwrap_or_else(|err| {
                    GrpcResponses::bad_request(stream_id, GrpcStatus::Unknown, err.description())
                })
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

#[cfg(test)]
mod tests {
    use std::io;
    use std::io::{Read, Write, Cursor};
    use std::sync::Arc;
    use std::time::Duration;

    use solicit::http::Header;
    use solicit::http::session::{Stream, DefaultStream};
    use solicit::http::transport::TransportStream;

    use super::*;
    use super::super::errors::GrpcError;

    #[test]
    fn test_parse_timeout() {
        assert_eq!(GrpcRequest::parse_timeout(&b"1H"[..]).unwrap().as_secs(),
                   60 * 60);
        assert_eq!(GrpcRequest::parse_timeout(&b"2M"[..]).unwrap().as_secs(),
                   2 * 60);
        assert_eq!(GrpcRequest::parse_timeout(&b"3S"[..]).unwrap().as_secs(), 3);
        assert_eq!(GrpcRequest::parse_timeout(&b"4m"[..]).unwrap().subsec_nanos(),
                   4_000_000);
        assert_eq!(GrpcRequest::parse_timeout(&b"5u"[..]).unwrap().subsec_nanos(),
                   5_000);
        assert_eq!(GrpcRequest::parse_timeout(&b"6n"[..]).unwrap().subsec_nanos(),
                   6);

        assert!(GrpcRequest::parse_timeout(&b"test"[..]).is_err());
    }

    #[test]
    fn test_request() {
        let mut stream = DefaultStream::with_id(123);

        assert!(GrpcRequest::new(&stream).is_err());

        stream.set_headers(vec![
            Header::new(&b":scheme"[..], &b"https"[..]),
            Header::new(&b":method"[..], &b"POST"[..]),
            Header::new(&b":path"[..], &b"/helloworld"[..]),
            Header::new(&b":authority"[..], &b"test.com"[..]),
            Header::new(&b"grpc-timeout"[..], &b"30S"[..]),
            Header::new(&b"content-type"[..], &b"application/grpc"[..]),
            Header::new(&b"grpc-encoding"[..], &b"gzip"[..]),
            Header::new(&b"grpc-accept-encoding"[..], &b"deflate"[..]),
            Header::new(&b"grpc-timeout"[..], &b"30S"[..]),
            Header::new(&b"user-agent"[..], &b"test"[..]),
            Header::new(&b"grpc-message-type"[..], &b"HelloWorld"[..]),
        ]);

        let req = GrpcRequest::new(&stream).unwrap();

        assert_eq!(req.stream_id, 123);
        assert_eq!(req.scheme, Some("https"));
        assert_eq!(req.method, Some("POST"));
        assert_eq!(req.path, Some("/helloworld"));
        assert_eq!(req.authority, Some("test.com"));
        assert_eq!(req.timeout, Some(Duration::new(30, 0)));
        assert_eq!(req.content_type, Some("application/grpc"));
        assert_eq!(req.encoding, Some("gzip"));
        assert_eq!(req.accept_encoding, Some("deflate"));
        assert_eq!(req.user_agent, Some("test"));
        assert_eq!(req.message_type, Some("HelloWorld"));
    }

    #[derive(Clone)]
    struct TestStream<T: Read + Write + Sized + Clone>(T);

    impl<T: Read + Write + Sized + Clone> Read for TestStream<T> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl<T: Read + Write + Sized + Clone> Write for TestStream<T> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl<T: Read + Write + Sized + Clone> TransportStream for TestStream<T> {
        fn try_split(&self) -> io::Result<Self> {
            Ok(TestStream(self.0.clone()))
        }

        fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_server_connection() {
        let mut router = Arc::new(GrpcRouter::default());
        let mut stream = TestStream(Cursor::new(Vec::new()));

        if let GrpcError::IoError(err) = GrpcServerConnection::new(router, stream).err().unwrap() {
            assert_eq!(err.kind(), io::ErrorKind::Other);
        } else {
            panic!()
        }
    }
}
