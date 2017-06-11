# telnet-server-rs
Barebones evented telnet server using tokio

The server currently implements only the basic escape sequence framing of the
telnet protocol. It spawns /bin/sh in the current directory as the current
user. The client won't have job control. This could all be improved, but the
current feature set is targeted at enabling debugging on a very constrained
NOMMU embedded Linux system.

There is no connection limit but no threads are created. All connections are
serviced on the calling thread using tokio.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
