Feature: Agent search
  A node finds service agents by the words of their listings, or looks one
  up by its full name. The best match comes first, and agents that are
  online come first inside one page. Only a contact of the agent gets its
  endpoint. A search counts against a limit for each account.

  In these scenarios, each listing and each search also has one word that
  is new for the scenario, so that listings from other scenarios do not
  match.

  Rule: A search finds listings by their words

    Scenario: Every word of the search must match
      Given both is listed with "contracts and leases"
      And one is listed with "contracts only"
      When a person searches for "leases"
      Then the search finds both
      When a person searches for all of them
      Then the search finds 2 agents

    Scenario: A better match comes first
      Given weak is listed with "other words about many things " 10 times and the tag "tax"
      And strong is listed with "tax", the skill "Tax" and the tag "tax"
      When a person searches for "tax"
      Then the search finds strong, weak

    Scenario: For the same match, the newer listing comes first
      Given it is 1 minute ago
      And older is listed with "same text"
      And it is now
      And newer is listed with "same text"
      When a person searches for "same"
      Then the search finds newer, older

    Scenario Outline: Words match as prefixes, without case and accents
      Given cafe is listed with "Café owner who bakes"
      When a person searches for "<word>"
      Then the search finds <result>

      Examples:
        | word  | result  |
        | caf   | cafe    |
        | cafe  | cafe    |
        | CAFÉ  | cafe    |
        | bak   | cafe    |
        | c     | cafe    |
        | bread | nothing |

    Scenario Outline: Search operators are plain words
      Given joined is listed with "law and tax"
      And plain is listed with "law tax"
      When a person searches for "<words>"
      Then the search finds <result>

      Examples:
        | words          | result  |
        | law AND tax    | joined  |
        | law and tax    | joined  |
        | "law" AND tax* | joined  |
        | (law) AND tax  | joined  |
        | NEAR law       | nothing |
        | name:law       | nothing |

    Scenario Outline: A search must have 2 to 8 words of a sane length and a limit of 1 to 20
      When a person searches with <search>
      Then the hub answers <status>

      Examples:
        | search                           | status |
        | a limit of 5 and no words        | 400    |
        | the word "a"                     | 400    |
        | only symbols                     | 400    |
        | 9 words                          | 400    |
        | a word of 129 letters            | 400    |
        | the word "law" and a limit of 0  | 400    |
        | the word "law" and a limit of 21 | 400    |
        | 8 words                          | 200    |
        | the word "law" and a limit of 20 | 200    |

    Scenario: A search with percent-encoded words passes the signature check
      Given cafe is listed with "café"
      When a person sends the search query "q={word}%20caf%C3%A9&limit=5"
      Then the search finds cafe

    Scenario: A search encoded as a node encodes free text passes the signature check
      # A node encodes a space as `+`, and `#`, `&` and other text with `%`.
      Given cafe is listed with "café"
      And both is listed with "café law tax"
      When a person sends the search query "q={word}+caf%C3%A9%23law%26tax"
      Then the search finds both

  Rule: The result shows presence, and the endpoint only to a contact

    Scenario: Agents that are online come first inside one page
      Given strong is listed with "tax tax tax" and the tag "tax"
      And weak is listed with "other words " 10 times
      And strong was last online long ago
      When a person searches for all of them
      Then the search finds weak online, then strong offline

    Scenario: Only a contact gets the endpoint
      Given law is listed with "contracts"
      And friend is a person with law as a contact
      When friend searches for all of them
      Then the search shows law as a contact, with its endpoint
      When a person searches for all of them
      Then the search shows law as no contact, without its endpoint
      And the row shows only the listing, its signature and expiry, the full name, the owner, and whether it is online and a contact

  Rule: A lookup by full name finds a listed service agent only

    Scenario Outline: A lookup by full name
      Given <agent> is <kind>
      When a person looks up <agent>
      Then the hub answers <status>

      Examples:
        | agent | kind                      | status |
        | law   | a listed service agent    | 200    |
        | lex   | an unlisted service agent | 404    |
        | ana   | a person                  | 404    |

    Scenario: A lookup answers with the agent's full name
      Given law is a listed service agent
      When a person looks up law
      Then the answer is the listing of law

    Scenario: A lookup of a bad name is invalid
      When a person looks up the name "Bad#0001"
      Then the hub answers 400

    Scenario: A node without a name cannot look up an agent
      Given law is a listed service agent
      And carol is a node without a name
      When carol looks up law
      Then the hub answers 403

  Rule: A node makes at most 30 searches and lookups in one minute

    Scenario: The 31st search in one minute is refused
      Given it is the start of a minute
      And a person
      When the person searches 30 times
      Then the hub answers 200 each time
      When the person searches once more
      Then the hub answers 429
      When the person searches for people
      Then the hub answers 429
