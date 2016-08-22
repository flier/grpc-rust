use std::collections::HashMap;

use protobuf;
use protobuf::compiler_plugin;
use protobuf::code_writer::CodeWriter;
use protobuf::descriptor::*;
use protobuf::descriptorx::*;

struct MethodGen<'a> {
    proto: &'a MethodDescriptorProto,
    service_path: String,
    root_scope: &'a RootScope<'a>,
}

impl<'a> MethodGen<'a> {
    fn new(proto: &'a MethodDescriptorProto, service_path: String, root_scope: &'a RootScope<'a>) -> MethodGen<'a> {
        MethodGen {
            proto: proto,
            service_path: service_path,
            root_scope: root_scope,
        }
    }

    fn input(&self) -> String {
        format!("super::{}", self.root_scope.find_message(self.proto.get_input_type()).rust_fq_name())
    }

    fn output(&self) -> String {
        format!("super::{}", self.root_scope.find_message(self.proto.get_output_type()).rust_fq_name())
    }

    fn sync_sig(&self) -> String {
        format!("{}(&self, p: {}) -> {}",
            self.proto.get_name(), self.input(), self.output())
    }

    fn async_sig(&self) -> String {
        // TODO: streams
        format!("{}(&self, p: {}) -> ::grpc::futures_grpc::GrpcFuture<{}>",
            self.proto.get_name(), self.input(), self.output())
    }

    fn write_sync_intf(&self, w: &mut CodeWriter) {
        w.fn_def(&self.sync_sig())
    }

    fn write_async_intf(&self, w: &mut CodeWriter) {
        w.fn_def(&self.async_sig())
    }

    fn write_sync_client(&self, w: &mut CodeWriter) {
        let input = self.root_scope.find_message(self.proto.get_input_type()).rust_fq_name();
        let output = self.root_scope.find_message(self.proto.get_output_type()).rust_fq_name();

        w.def_fn(&self.sync_sig(), |w| {
            self.write_method_descriptor(w,
                &format!("let method: ::grpc::method::MethodDescriptor<{}, {}> = ", self.input(), self.output()),
                ";");
            w.write_line("self.grpc_client.borrow_mut().call(p, method)")
        });
    }

    fn write_async_client(&self, w: &mut CodeWriter) {
        let input = self.root_scope.find_message(self.proto.get_input_type()).rust_fq_name();
        let output = self.root_scope.find_message(self.proto.get_output_type()).rust_fq_name();

        w.def_fn(&self.async_sig(), |w| {
            self.write_method_descriptor(w,
                &format!("let method: ::grpc::method::MethodDescriptor<{}, {}> = ", self.input(), self.output()),
                ";");
            w.write_line("self.grpc_client_async.call(p, method)")
        });
    }

    fn write_method_descriptor(&self, w: &mut CodeWriter, before: &str, after: &str) {
        w.block(format!("{}{}", before, "::grpc::method::MethodDescriptor {"), format!("{}{}", "}", after), |w| {
            w.field_entry("name", format!("\"{}/{}\".to_string()", self.service_path, self.proto.get_name()));
            w.field_entry("client_streaming", if self.proto.get_client_streaming() { "true" } else { "false" });
            w.field_entry("server_streaming", if self.proto.get_server_streaming() { "true" } else { "false" });
            w.field_entry("req_marshaller", "Box::new(::grpc::grpc_protobuf::MarshallerProtobuf)");
            w.field_entry("resp_marshaller", "Box::new(::grpc::grpc_protobuf::MarshallerProtobuf)");
        });
    }
}

struct ServiceGen<'a> {
    proto: &'a ServiceDescriptorProto,
    root_scope: &'a RootScope<'a>,
    methods: Vec<MethodGen<'a>>,
    service_path: String,
    package: String,
}

impl<'a> ServiceGen<'a> {
    fn new(proto: &'a ServiceDescriptorProto, file: &FileDescriptorProto, root_scope: &'a RootScope) -> ServiceGen<'a> {
        let service_path = format!("/{}.{}", file.get_package(), proto.get_name());
        let methods = proto.get_method().into_iter()
            .map(|m| MethodGen::new(m, service_path.clone(), root_scope))
            .collect();

        ServiceGen {
            proto: proto,
            root_scope: root_scope,
            methods: methods,
            service_path: service_path,
            package: file.get_package().to_string(),
        }
    }

    // name of synchronous interface
    fn sync_intf_name(&self) -> &str {
        self.proto.get_name()
    }

    // name of asynchronous interface
    fn async_intf_name(&self) -> String {
        format!("{}Async", self.sync_intf_name())
    }

    fn sync_client_name(&self) -> String {
        format!("{}Client", self.sync_intf_name())
    }

    fn sync_server_name(&self) -> String {
        format!("{}Server", self.sync_intf_name())
    }

    fn async_client_name(&self) -> String {
        format!("{}Client", self.async_intf_name())
    }

    fn async_server_name(&self) -> String {
        format!("{}Server", self.async_intf_name())
    }

    fn write_sync_intf(&self, w: &mut CodeWriter) {
        w.pub_trait(&self.sync_intf_name(), |w| {
            for (i, method) in self.methods.iter().enumerate() {
                if i != 0 {
                    w.write_line("");
                }

                method.write_sync_intf(w);
            }
        });
    }

    fn write_async_intf(&self, w: &mut CodeWriter) {
        w.pub_trait(&self.async_intf_name(), |w| {
            for (i, method) in self.methods.iter().enumerate() {
                if i != 0 {
                    w.write_line("");
                }

                method.write_async_intf(w);
            }
        });
    }

    fn write_sync_client(&self, w: &mut CodeWriter) {
        w.pub_struct(&self.sync_client_name(), |w| {
            w.field_decl("grpc_client", "::std::cell::RefCell<::grpc::client::GrpcClient>");
        });

        w.write_line("");

        w.impl_self_block(&self.sync_client_name(), |w| {
            w.pub_fn("new(host: &str, port: u16) -> Self", |w| {
                w.expr_block(&self.sync_client_name(), |w| {
                    w.field_entry("grpc_client", "::std::cell::RefCell::new(::grpc::client::GrpcClient::new(host, port))");
                });
            });
        });

        w.write_line("");

        w.impl_for_block(self.sync_intf_name(), &self.sync_client_name(), |w| {
            for (i, method) in self.methods.iter().enumerate() {
                if i != 0 {
                    w.write_line("");
                }

                method.write_sync_client(w);
            }
        });
    }

    fn write_async_client(&self, w: &mut CodeWriter) {
        w.pub_struct(&self.async_client_name(), |w| {
            w.field_decl("grpc_client_async", "::grpc::client_async::GrpcClientAsync");
        });

        w.write_line("");

        w.impl_self_block(&self.async_client_name(), |w| {
            w.pub_fn("new(host: &str, port: u16) -> Self", |w| {
                w.expr_block(&self.async_client_name(), |w| {
                    w.field_entry("grpc_client_async", "::grpc::client_async::GrpcClientAsync::new(host, port)");
                });
            });
        });

        w.write_line("");

        w.impl_for_block(self.async_intf_name(), &self.async_client_name(), |w| {
            for (i, method) in self.methods.iter().enumerate() {
                if i != 0 {
                    w.write_line("");
                }

                method.write_async_client(w);
            }
        });
    }

    fn write_server(&self, w: &mut CodeWriter) {
        w.pub_struct(&self.sync_server_name(), |w| {
            w.field_decl("server", "::grpc::server::GrpcServer");
        });

        w.write_line("");

        w.impl_self_block(&self.sync_server_name(), |w| {
            w.pub_fn(format!("new<H : {} + 'static + Sync + Send>(h: H) -> {}", self.sync_intf_name(), self.sync_server_name()), |w| {
                w.write_line("let handler_arc = ::std::sync::Arc::new(h);");
                w.block("let service_definition = ::std::sync::Arc::new(::grpc::method::ServerServiceDefinition::new(", "));", |w| {
                    w.block("vec![", "],", |w| {
                        for method in &self.methods {
                            w.block("::grpc::method::ServerMethod::new(", "),", |w| {
                                method.write_method_descriptor(w, "", ",");
                                w.block("{", "},", |w| {
                                    w.write_line("let handler_copy = handler_arc.clone();");
                                    w.write_line(format!("::grpc::method::MethodHandlerFn::new(move |p| handler_copy.{}(p))", method.proto.get_name()));
                                });
                            });
                        }
                    });
                });

                w.expr_block(self.sync_server_name(), |w| {
                    w.field_entry("server", "::grpc::server::GrpcServer::new(service_definition)");
                });
            });

            w.write_line("");

            w.pub_fn("run(&mut self)", |w| {
                w.write_line("self.server.run()")
            });
        });
    }

    fn write(&self, w: &mut CodeWriter) {
        w.comment("interface");
        w.write_line("");
        self.write_sync_intf(w);
        w.write_line("");
        self.write_async_intf(w);
        w.write_line("");
        w.comment("sync client");
        w.write_line("");
        self.write_sync_client(w);
        w.write_line("");
        w.comment("async client");
        w.write_line("");
        self.write_async_client(w);
        w.write_line("");
        w.comment("sync server");
        w.write_line("");
        self.write_server(w);
    }
}

fn gen_file(
    file: &FileDescriptorProto,
    root_scope: &RootScope,
) -> Option<compiler_plugin::GenResult>
{
    if file.get_service().is_empty() {
        return None;
    }

    let base = protobuf::descriptorx::proto_path_to_rust_mod(file.get_name());

    let mut v = Vec::new();
    {
        let mut w = CodeWriter::new(&mut v);
        w.write_generated();
        w.write_line("");

        for service in file.get_service() {
            w.write_line("");
            ServiceGen::new(service, file, root_scope).write(&mut w);
        }
    }

    Some(compiler_plugin::GenResult {
        name: base + "_grpc.rs",
        content: v,
    })
}

pub fn gen(file_descriptors: &[FileDescriptorProto], files_to_generate: &[String])
        -> Vec<compiler_plugin::GenResult>
{
    let files_map: HashMap<&str, &FileDescriptorProto> =
        file_descriptors.iter().map(|f| (f.get_name(), f)).collect();

    let root_scope = RootScope { file_descriptors: file_descriptors };

    let mut results = Vec::new();

    for file_name in files_to_generate {
        let file = files_map[&file_name[..]];

        if file.get_service().is_empty() {
            continue;
        }

        results.extend(gen_file(file, &root_scope).into_iter());
    }

    results
}

pub fn protoc_gen_grpc_rust_main() {
    compiler_plugin::plugin_main(gen);
}
