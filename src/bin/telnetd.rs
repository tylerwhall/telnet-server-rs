extern crate env_logger;
extern crate telnet;

fn main() {
    env_logger::init().unwrap();
    telnet::telnet_serve("::0:2323");
}
