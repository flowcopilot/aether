Feature: Rate limits
  The hub is shared by many nodes. Rate limits stop one node or one
  address from using up the hub, and stop a flood of new accounts. Each
  limit counts in a fixed time window, and each kind of request counts on
  its own.

  Scenario: A node can send 120 requests a minute
    Given alice is a new node
    When alice sends 121 requests in one minute
    Then the hub answers the first 120 and refuses the last with 429
    And the next request of alice gets the error "rate_limited"
    And the hub still answers another node

  Scenario: One address can make 20 new accounts an hour
    Given an address that made no accounts this hour
    When 20 new nodes claim a name from that address
    Then the hub accepts all 20 claims
    And a 21st new node from that address gets 429 with the error "rate_limited"
    And a new node from another address can claim a name

  Scenario: A claim again by an existing account is not a new account
    Given an address that made no accounts this hour
    When one new node claims the same name 25 times from that address
    Then the hub accepts all 25 claims
    When 19 new nodes claim a name from that address
    Then the hub accepts all 19 claims
    And a new node from that address gets 429

  Scenario: A refused request does not mark the node as seen
    Given alice is a new node
    And alice sent 120 requests in one minute
    And alice was last seen long ago
    When alice sends one more request
    Then the hub answers 429
    And alice is still last seen long ago

  Scenario: Searches and other requests are counted apart
    Given a key that made 30 searches this minute
    Then a 31st search of the key is refused
    But a request of another kind from the key is allowed
