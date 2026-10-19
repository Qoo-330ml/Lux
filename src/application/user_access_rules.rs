use std::{fmt, net::IpAddr, str::FromStr};

const MAX_USER_ACCESS_RULE_VALUE_BYTES: usize = 256;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserAccessBlacklistRuleType {
    Device,
    Client,
    IpCidr,
}

#[allow(dead_code)]
impl UserAccessBlacklistRuleType {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "DEVICE" => Some(Self::Device),
            "CLIENT" => Some(Self::Client),
            "IP_CIDR" => Some(Self::IpCidr),
            _ => None,
        }
    }

    pub(crate) fn as_database_value(self) -> &'static str {
        match self {
            Self::Device => "DEVICE",
            Self::Client => "CLIENT",
            Self::IpCidr => "IP_CIDR",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserAccessBlacklistRuleValidationError {
    Empty,
    TooLong,
    ControlCharacter,
    InvalidIpAddress,
    InvalidPrefix,
    PrefixOutOfRange,
}

impl fmt::Display for UserAccessBlacklistRuleValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "rule value must not be empty",
            Self::TooLong => "rule value exceeds 256 bytes",
            Self::ControlCharacter => "rule value contains a control character",
            Self::InvalidIpAddress => "rule value must be an IPv4 or IPv6 address",
            Self::InvalidPrefix => "rule value contains an invalid network prefix",
            Self::PrefixOutOfRange => "network prefix is outside the address family range",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for UserAccessBlacklistRuleValidationError {}

#[allow(dead_code)]
pub(crate) fn normalize_user_access_blacklist_value(
    rule_type: UserAccessBlacklistRuleType,
    value: &str,
) -> Result<String, UserAccessBlacklistRuleValidationError> {
    validate_rule_text(value)?;
    match rule_type {
        UserAccessBlacklistRuleType::Device => Ok(value.to_owned()),
        UserAccessBlacklistRuleType::Client => {
            let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                return Err(UserAccessBlacklistRuleValidationError::Empty);
            }
            let normalized = normalized.to_lowercase();
            validate_rule_text(&normalized)?;
            Ok(normalized)
        }
        UserAccessBlacklistRuleType::IpCidr => Ok(IpCidr::parse(value)?.to_string()),
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

#[allow(dead_code)]
impl IpCidr {
    pub(crate) fn parse(value: &str) -> Result<Self, UserAccessBlacklistRuleValidationError> {
        validate_rule_text(value)?;
        let value = value.trim();
        let (address_text, prefix_text) = match value.split_once('/') {
            Some((address_text, prefix_text)) if !prefix_text.contains('/') => {
                (address_text, Some(prefix_text))
            }
            Some(_) => return Err(UserAccessBlacklistRuleValidationError::InvalidPrefix),
            None => (value, None),
        };
        let address = IpAddr::from_str(address_text)
            .map_err(|_| UserAccessBlacklistRuleValidationError::InvalidIpAddress)?;
        let default_prefix = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix = match prefix_text {
            Some(prefix_text)
                if !prefix_text.is_empty()
                    && prefix_text.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                prefix_text
                    .parse::<u8>()
                    .map_err(|_| UserAccessBlacklistRuleValidationError::PrefixOutOfRange)?
            }
            Some(_) => return Err(UserAccessBlacklistRuleValidationError::InvalidPrefix),
            None => default_prefix,
        };
        let max_prefix = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix > max_prefix {
            return Err(UserAccessBlacklistRuleValidationError::PrefixOutOfRange);
        }

        Ok(Self {
            network: network_address(address, prefix),
            prefix,
        })
    }

    pub(crate) fn contains(self, address: IpAddr) -> bool {
        network_address(address, self.prefix) == self.network
    }
}

#[allow(dead_code)]
impl fmt::Display for IpCidr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.network, self.prefix)
    }
}

fn validate_rule_text(value: &str) -> Result<(), UserAccessBlacklistRuleValidationError> {
    if value.len() > MAX_USER_ACCESS_RULE_VALUE_BYTES {
        return Err(UserAccessBlacklistRuleValidationError::TooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(UserAccessBlacklistRuleValidationError::ControlCharacter);
    }
    if value.trim().is_empty() {
        return Err(UserAccessBlacklistRuleValidationError::Empty);
    }
    Ok(())
}

fn network_address(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(address) => {
            let address = u32::from(address);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            IpAddr::V4((address & mask).into())
        }
        IpAddr::V6(address) => {
            let address = u128::from(address);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            IpAddr::V6((address & mask).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IpCidr, UserAccessBlacklistRuleType, UserAccessBlacklistRuleValidationError,
        normalize_user_access_blacklist_value,
    };
    use std::net::IpAddr;

    #[test]
    fn device_ids_keep_exact_match_semantics_and_reject_invalid_text() {
        assert_eq!(
            normalize_user_access_blacklist_value(
                UserAccessBlacklistRuleType::Device,
                " Device/With Case ",
            ),
            Ok(" Device/With Case ".to_owned())
        );
        assert_eq!(
            normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::Device, "   "),
            Err(UserAccessBlacklistRuleValidationError::Empty)
        );
        assert_eq!(
            normalize_user_access_blacklist_value(
                UserAccessBlacklistRuleType::Device,
                "device\nname",
            ),
            Err(UserAccessBlacklistRuleValidationError::ControlCharacter)
        );
        assert_eq!(
            normalize_user_access_blacklist_value(
                UserAccessBlacklistRuleType::Device,
                &"x".repeat(257),
            ),
            Err(UserAccessBlacklistRuleValidationError::TooLong)
        );
    }

    #[test]
    fn client_names_collapse_whitespace_and_ignore_case() {
        assert_eq!(
            normalize_user_access_blacklist_value(
                UserAccessBlacklistRuleType::Client,
                "  InFUse   Player   TV  ",
            ),
            Ok("infuse player tv".to_owned())
        );
        assert_eq!(
            normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::Client, "   "),
            Err(UserAccessBlacklistRuleValidationError::Empty)
        );
        assert_eq!(
            normalize_user_access_blacklist_value(
                UserAccessBlacklistRuleType::Client,
                "Infuse\u{0007}Player",
            ),
            Err(UserAccessBlacklistRuleValidationError::ControlCharacter)
        );
    }

    #[test]
    fn ip_addresses_and_networks_are_canonicalized() {
        for (input, expected) in [
            ("192.0.2.129/24", "192.0.2.0/24"),
            ("192.0.2.42", "192.0.2.42/32"),
            ("0.0.0.0/0", "0.0.0.0/0"),
            ("2001:db8:0:0:0:0:0:1/64", "2001:db8::/64"),
            ("2001:db8::1/32", "2001:db8::/32"),
            ("2001:db8::1", "2001:db8::1/128"),
            ("::/0", "::/0"),
        ] {
            assert_eq!(
                normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::IpCidr, input,),
                Ok(expected.to_owned()),
                "input {input}"
            );
        }
    }

    #[test]
    fn ip_cidr_contains_only_addresses_in_its_network() {
        let ipv4 = IpCidr::parse("192.0.2.128/25").expect("valid IPv4 CIDR");
        assert!(ipv4.contains("192.0.2.255".parse::<IpAddr>().unwrap()));
        assert!(!ipv4.contains("192.0.2.127".parse::<IpAddr>().unwrap()));
        assert!(!ipv4.contains("2001:db8::1".parse::<IpAddr>().unwrap()));

        let ipv6 = IpCidr::parse("2001:db8::abcd/64").expect("valid IPv6 CIDR");
        assert!(ipv6.contains("2001:db8::1".parse::<IpAddr>().unwrap()));
        assert!(!ipv6.contains("2001:db8:1::1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn invalid_ip_addresses_and_prefixes_are_rejected() {
        for input in [
            "",
            "not-an-ip",
            "192.0.2.1/33",
            "2001:db8::1/129",
            "192.0.2.1/128",
            "192.0.2.1/24/1",
            "192.0.2.1/",
            "192.0.2.1/+24",
        ] {
            assert!(
                normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::IpCidr, input,)
                    .is_err(),
                "accepted invalid input {input:?}"
            );
        }
        assert_eq!(
            normalize_user_access_blacklist_value(
                UserAccessBlacklistRuleType::IpCidr,
                &"1".repeat(257),
            ),
            Err(UserAccessBlacklistRuleValidationError::TooLong)
        );
    }
}
