extern crate env_logger;
extern crate telnet_server;

fn main() {
    env_logger::init().unwrap();
    telnet_server::telnet_serve("::0:2323");
}
