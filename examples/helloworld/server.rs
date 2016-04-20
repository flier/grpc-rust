#[macro_use]
extern crate log;
extern crate env_logger;

extern crate protobuf;
extern crate grpc;

use std::thread;
use std::sync::Arc;

use grpc::server::{GrpcRouter, GrpcServer, TcpServer};

mod helloworld;

use helloworld::{HelloRequest, HelloReply, Greeter};

impl Greeter for TcpServer {
    fn SayHello(req: HelloRequest) -> ::protobuf::ProtobufResult<HelloReply> {
        let mut res = HelloReply::new();

        res.set_message(format!("hello {}", req.get_name()));

        Ok(res)
    }
}

fn main() {
    env_logger::init().unwrap();

    let router = GrpcRouter::new();

    TcpServer::new(Arc::new(router), "127.0.0.1:50051").unwrap().run(|mut conn| {
        thread::spawn(move || conn.run());
    });
}
