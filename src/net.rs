//! Address identities shared by every per-address limit and sanction.

use std::net::{IpAddr, Ipv6Addr};

/// The identity an address is limited, banned and muted by: the exact IPv4
/// address, an IPv4-mapped IPv6 address as that IPv4 address, and any other
/// IPv6 address as its /64. Rotating through a /64 is free for most IPv6
/// subscribers, so a narrower identity would let a limited or banned guest
/// return at once.
pub fn cohort(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return IpAddr::V4(mapped);
            }
            let segments = address.segments();
            IpAddr::V6(Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn ipv6_addresses_share_their_slash_64() {
        assert_eq!(
            cohort(ip("2001:db8:1:2::1")),
            cohort(ip("2001:db8:1:2:ffff::42"))
        );
        assert_eq!(cohort(ip("2001:db8:1:2::1")), ip("2001:db8:1:2::"));
        assert_ne!(cohort(ip("2001:db8:1:2::1")), cohort(ip("2001:db8:1:3::1")));
    }

    #[test]
    fn ipv4_addresses_stay_exact_and_mapped_ones_unwrap() {
        assert_eq!(cohort(ip("192.0.2.1")), ip("192.0.2.1"));
        assert_ne!(cohort(ip("192.0.2.1")), cohort(ip("192.0.2.2")));
        assert_eq!(cohort(ip("::ffff:192.0.2.1")), ip("192.0.2.1"));
        assert_ne!(
            cohort(ip("::ffff:192.0.2.1")),
            cohort(ip("::ffff:192.0.2.2"))
        );
    }
}
