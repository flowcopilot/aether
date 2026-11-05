Feature: Client address behind a proxy
  The hub limits requests by client address. Behind a trusted proxy, the
  client is the right-most address that the proxy added to
  `x-forwarded-for`. The hub does not believe the header from any other peer.

  Background:
    Given the trusted proxies "10.0.1.0/24, fd00::/8"

  Scenario: A peer that is not a trusted proxy is the client
    When "203.0.113.7" sends a request forwarded for "198.51.100.1"
    Then the client address is "203.0.113.7"

  Scenario: Behind a trusted proxy, the right-most address the proxy added is the client
    When "10.0.1.5" sends a request forwarded for "198.51.100.1, 203.0.113.9"
    Then the client address is "203.0.113.9"

  Scenario: Trusted addresses in the header are skipped, also across more than one header
    When "10.0.1.5" sends a request with these forwarded headers:
      | forwarded for         |
      | 198.51.100.1          |
      | 203.0.113.9, 10.0.1.7 |
    Then the client address is "203.0.113.9"

  Scenario: A trusted IPv6 proxy works the same way
    When "fd00::1" sends a request forwarded for "2001:db8::5"
    Then the client address is "2001:db8::5"

  Scenario Outline: A bad entry in the header is skipped
    When "10.0.1.5" sends a request forwarded for "<forwarded>"
    Then the client address is "<client>"

    Examples:
      | forwarded         | client      |
      | 203.0.113.9, junk | 203.0.113.9 |
      | junk, 10.0.1.8    | 10.0.1.5    |

  Scenario: Without the header, the proxy is the client
    When "10.0.1.5" sends a request with no forwarded header
    Then the client address is "10.0.1.5"

  Scenario Outline: The operator gives trusted proxies as IPv4 or IPv6 ranges
    When the operator sets the trusted proxies to "<ranges>"
    Then the hub trusts <count> ranges

    Examples:
      | ranges                           | count |
      |                                  | 0     |
      | 10.0.0.0/8,192.168.1.1, ::1/128  | 3     |

  Scenario Outline: A bad proxy range is refused at startup
    When the operator sets the trusted proxies to "<range>"
    Then the hub refuses the setting

    Examples:
      | range        |
      | 10.0.0.0/33  |
      | not-an-ip    |
      | 10.0.0.0/8/1 |
      | fd00::/129   |
