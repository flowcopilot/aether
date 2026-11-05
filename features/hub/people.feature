Feature: Finding people
  A person who knows the exact tag of a personal agent, such as
  `bob#0042`, can look it up before they send an invite. A person can
  also let others find their personal agent by the start of its name.
  Only contacts see if the agent is online and where to reach it. Service
  agents are found in the directory, not here.

  Scenario: An exact lookup finds a personal agent that is not searchable
    Given alice has a name
    And bob has a name
    And bob has read his invites
    When alice looks up the tag of bob
    Then the hub answers 200
    And alice finds bob, not as a contact
    And the invite version of bob did not move

  Scenario: A node without an account cannot look up a tag
    Given bob has a name
    And zed is a new node
    When zed looks up the tag of bob
    Then the hub answers 403

  Scenario: Only a contact sees if the agent is online and where it is
    Given alice has a name
    And bob has a name
    And carol has a name
    And alice and bob are contacts
    When alice looks up the tag of bob
    Then alice finds bob as a contact that is online, with his key
    When carol looks up the tag of bob
    Then carol finds bob, not as a contact

  Scenario: A lookup does not find a service agent
    Given alice has a name
    And shop has a service name
    When alice looks up the tag of shop
    Then the hub answers 404

  Scenario: A lookup does not find a tag that nobody has
    Given alice has a name
    And bob has a name
    When alice looks up the name of bob with another number
    Then the hub answers 404 with the error "not_found"

  Scenario Outline: A lookup with a bad name or number is invalid
    Given alice has a name
    When alice looks up the name "<name>" with the number "<number>"
    Then the hub answers 400 with the error "invalid"

    Examples:
      | name | number | problem                  |
      | Ana  | 0042   | the name has upper case  |
      | ana  | 42     | the number has 2 digits  |
      | ana  | 0000   | the number is zero       |
      | ana  | 00420  | the number has 5 digits  |
      | -ana | 0042   | the name starts with "-" |

  Scenario Outline: A search with a bad search word or limit is invalid
    Given alice has a name
    When alice searches for "<word>" with a limit of <limit>
    Then the hub answers 400

    Examples:
      | word                              | limit | problem                   |
      |                                   | 5     | no search word            |
      | a                                 | none  | the word is too short     |
      | aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | none  | the word is too long      |
      | Ana                               | none  | the word has upper case   |
      | a#b                               | none  | the word has a "#"        |
      | ab                                | 0     | the limit is too small    |
      | ab                                | 21    | the limit is too large    |
      | ab                                | x     | the limit is not a number |

  Scenario: A search can ask for up to 20 matches
    Given alice has a name
    When alice searches for "ab" with a limit of 20
    Then the hub answers 200

  Scenario: A node can make 30 searches and lookups a minute
    Given it is the start of a minute
    And alice has a name
    And bob has a name
    When alice looks up the tag of bob 15 times and searches for his name 15 times
    Then the hub answers all 30
    When alice looks up the tag of bob
    Then the hub answers 429 with the error "rate_limited"
    And a search of alice for the name of bob gets 429
    And another node can still look up the tag of bob

  Rule: A search lists the searchable personal agents whose name starts with the search word
    Background:
      Given a new search word
      And these accounts, whose names use the search word:
        | name    | type     | searchable |
        | {word}b | personal | yes        |
        | {word}a | personal | yes        |
        | {word}c | personal | no         |
        | {word}d | service  | no         |
        | x{word} | personal | yes        |
      And alice has a name
      And alice and {word}b are contacts

    Scenario: A search lists the matches by handle, and only a contact shows its details
      When alice searches for "{word}"
      Then the hub answers 200
      And alice finds, in this order:
        | name    | contact |
        | {word}a | no      |
        | {word}b | yes     |

    Scenario: A search returns at most the number of matches asked for
      When alice searches for "{word}" with a limit of 1
      Then alice finds, in this order:
        | name    | contact |
        | {word}a | no      |

    Scenario: A search with no match finds nobody
      When alice searches for "{word}z"
      Then alice finds nobody
