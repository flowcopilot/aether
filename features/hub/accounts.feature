Feature: Hub accounts
  A node claims a name on the hub, such as `alice`. The hub adds a number,
  so that many people can use the same name: the tag `alice#0042` belongs
  to one key only. The account also has a type: a personal agent belongs
  to a person, a service agent offers a service. A person can let others
  find their personal agent in search.

  Scenario: A node claims a name and the hub picks its number
    Given alice is a new node
    And alice has no account
    When alice claims a new name
    Then the hub answers 200
    And the account has that name and a number from 1 to 9999
    And the account handle is the name with the number
    And the account belongs to the key of alice
    And the account is a personal agent that is not searchable
    And alice reads back the same account

  Scenario: A claim without a type makes a personal agent
    Given alice is a new node
    When alice claims a new name
    Then the account is a personal agent

  Scenario: Two nodes can share a name with different numbers
    When two new nodes claim the same name
    Then they get different numbers

  Scenario: The same claim again keeps the number
    Given alice has a name
    When alice claims her name again
    Then alice keeps her number

  Scenario: A rename keeps the number when it is free for the new name
    Given alice has a name
    When alice renames herself to a new name
    Then alice has the new name with her old number

  Scenario: A rename keeps the number when it is free, also when the key released another number of the new name
    Given alice has a name
    And alice once released another number of a new name
    When alice renames herself to that name
    Then alice keeps her number

  Scenario: A rename takes the number the key released when its own number is taken
    Given alice has a name
    And another key holds the number of alice with a new name
    And alice once released another number of that name
    When alice renames herself to that name
    Then alice gets the number she released

  Scenario: A rename picks another number when the current one is taken
    Given alice has a name
    And another key holds the number of alice with a new name
    When alice renames herself to that name
    Then alice has the new name with another number

  Scenario: A released tag never goes to another key
    Given alice has a name
    And bob is a new node
    When alice renames herself to a new name
    Then the hub answers 200
    And the hub does not offer the old tag of alice to bob
    And bob gets another number when he claims the old name of alice
    And the hub still offers the old tag of alice to alice

  Scenario: The type is set at the first claim and does not change
    Given carol has a service name
    When carol changes her type to personal
    Then the hub answers 400
    When carol changes her type to service
    Then the hub answers 200
    When carol renames herself to a new name
    Then the account is a service agent

  Scenario: A service agent cannot be searchable when it claims its name
    Given carol is a new node
    When carol claims a new service name that is searchable
    Then the hub answers 400

  Scenario: A service agent cannot become searchable later
    Given carol has a service name
    When carol makes herself searchable
    Then the hub answers 400

  Scenario: A personal agent becomes searchable and keeps its name and number
    Given alice has a name
    When alice makes herself searchable
    Then the account is searchable
    And alice keeps her name and number

  Scenario Outline: A name that breaks the name rules is refused
    When a new node claims the name <name>
    Then the hub answers 400

    Examples:
      | name                                | rule                      |
      | ""                                  | not empty                 |
      | "Bob"                               | lower case only           |
      | "-bob"                              | starts with a letter or digit |
      | ".bob"                              | starts with a letter or digit |
      | "bob!"                              | letters, digits, . _ -    |
      | "bob#0001"                          | no number in the name     |
      | "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" | at most 32 characters     |
      | 7                                   | a string                  |

  Scenario Outline: A name that follows the name rules is accepted
    When a new node claims the name <name>
    Then the hub answers 200

    Examples:
      | name                               |
      | "b"                                |
      | "0bob"                             |
      | "bob.smith_2-x"                    |
      | "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" |

  Scenario Outline: A claim with other content than a name and a type is refused
    When a new node sends a claim with <content>
    Then the hub answers 400

    Examples:
      | content                    |
      | a handle instead of a name |
      | the unknown type robot     |
      | no fields                  |
      | a body that is not JSON    |
      | a JSON list                |

  Scenario: A node can rename five times a day
    Given alice has a name
    When alice renames herself 6 times
    Then the first 5 renames pass and the sixth gets 429
    And alice can still claim her current name again

  Scenario: Deleting an account removes it and wakes the other side
    Given alice has a name
    And bob has a name
    And alice invited bob
    And bob has read his invites
    When alice deletes her account
    Then the hub answers 204
    And alice has no account
    And bob has no invites, and his invite version moved on

  Scenario: A deleted account cannot be deleted again
    Given alice has a name
    And alice deleted her account
    When alice deletes her account
    Then the hub answers 403

  Scenario: The same key gets its old tag back after a deletion
    Given alice has a name
    And alice deleted her account
    When alice claims her name again
    Then alice keeps her number

  Scenario Outline: After a deletion, the key keeps its type
    Given carol has a service name
    And carol deleted her account
    When carol claims her name again <type>
    Then the hub answers <status>

    Examples:
      | type                 | status |
      | as a personal agent  | 400    |
      | without a type       | 400    |
      | as a service agent   | 200    |
