#[macro_use]
extern crate log;
extern crate env_logger;

extern crate protobuf;
extern crate grpc;

use std::thread;
use std::net::ToSocketAddrs;

use grpc::errors::GrpcResult;
use grpc::server::{GrpcServer, TcpServer, SslServer};

mod helloworld;

use helloworld::{HelloRequest, HelloReply, Greeter};

impl Greeter for TcpServer {
    fn SayHello(req: HelloRequest) -> ::protobuf::ProtobufResult<HelloReply> {
        unimplemented!()
    }
}

fn main() {
    env_logger::init().unwrap();

    let tcpServer = thread::spawn(move || TcpServer::new("127.0.0.1:50051").unwrap().run());
    let sslServer = thread::spawn(move || SslServer::new("127.0.0.1:50052").unwrap().run());

    tcpServer.join();
    sslServer.join();
}
