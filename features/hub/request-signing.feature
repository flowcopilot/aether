Feature: Signed requests
  A node proves who it is with its key. It signs each request to the hub:
  the method, the path with its query, the time and the body. The hub
  refuses a request that is not signed, signed by another key, too old or
  changed on the way. The node and the hub must also speak the same
  protocol.

  Scenario: The health check needs no signature
    When a monitor asks the hub for its health
    Then the hub answers 200 with "ok"

  Scenario: A request without a signature is refused
    When a node sends a request without a signature
    Then the hub answers 401 with the error "unauthorized"

  Scenario: A correctly signed request passes
    When a node without an account sends a correctly signed request
    Then the hub answers 404

  Scenario: A correctly signed request with a query passes
    Given alice has a name
    When alice reads her invites with a query in a correctly signed request
    Then the hub answers 200

  Scenario Outline: A request that is not correctly signed is refused
    Given alice has a name
    When alice sends a request <problem>
    Then the hub answers 401

    Examples:
      | problem                                |
      | signed by another key                  |
      | with a malformed signature             |
      | signed 301 seconds ago                 |
      | signed 301 seconds in the future       |
      | signed over another body               |
      | signed for another query               |

  Scenario Outline: A node on another protocol is told to upgrade
    When a node sends a request <protocol>
    Then the hub answers 426 with the error "upgrade_required"

    Examples:
      | protocol                    |
      | without a protocol version  |
      | for protocol 1              |
      | for protocol 3              |

  Scenario Outline: Any node can ask which protocol the hub speaks
    When a node without an account asks the hub for its protocol <protocol>
    Then the hub answers that it speaks protocol 2

    Examples:
      | protocol                   |
      | as protocol 2              |
      | without a protocol version |
