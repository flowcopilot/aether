Feature: Listings
  A service agent publishes a listing so that people can find it. The node
  signs the listing text with its key, and the hub keeps and returns the
  exact bytes, so that anyone can check the signature. The hub accepts only
  a short, plain listing with known fields. A listing ends when it expires,
  when the agent deletes it, or when the agent changes its name or deletes
  its account.

  Background:
    Given law is a service agent

  Rule: The hub keeps the listing text exactly as the node signed it

    Scenario: The hub returns the listing text exactly as the node sent it
      When law publishes this listing text:
        """
        { "skills": [{"tags": ["law"], "name": "Contracts", "id": "general", "description": "Reads contracts."}], "description": "A lawyer's agent.",  "name": "{full name}" }
        """
      Then the hub answers 200 with the same bytes
      And law reads its listing with the same text and a signature
      And a person who looks up law gets the same text and signature

  Rule: Only a service agent with a name can publish a listing

    Scenario: A personal agent cannot list itself
      Given alice is a person
      When alice publishes a listing
      Then the hub answers 400 with the error "invalid"

    Scenario: A node without a name cannot publish a listing
      Given carol is a node without a name
      When carol publishes an empty listing
      Then the hub answers 403

  Rule: The signature and the expiry must be valid

    Scenario Outline: A listing with a bad signature or expiry is refused
      When law publishes its listing <problem>
      Then the hub answers 400
      And law has no listing

      Examples:
        | problem                                     |
        | signed by another key                       |
        | with text that differs from the signed text |
        | that expired a second ago                   |
        | that expires in 31 days                     |
        | marked as not listed                        |
        | with the expiry as text                     |
        | with a malformed signature                  |
        | with an extra field                         |
        | with text that is not base64url             |

    Scenario: A listing can last up to 30 days
      When law publishes its listing that expires in 30 days
      Then the hub answers 200

  Rule: A listing is short and has only known fields

    Scenario Outline: A listing outside the allowlist is refused
      When law publishes a listing with <problem>
      Then the hub answers 400

      Examples:
        | problem                               |
        | an extra key                          |
        | no description                        |
        | an empty description                  |
        | a description of 501 characters       |
        | a control character                   |
        | a bidirectional character             |
        | a zero-width character                |
        | no skill                              |
        | six skills                            |
        | two skills with one id                |
        | an extra skill key                    |
        | a bad skill id                        |
        | a skill name of 65 characters         |
        | a skill description of 301 characters |
        | no tag                                |
        | six tags                              |
        | a tag two times                       |
        | an upper-case tag                     |
        | a tag of 25 characters                |
        | a number as the description           |
        | one key two times                     |
        | a list instead of an object           |
        | text that is not JSON                 |
        | more than 8 KiB of text               |

    Scenario: A description can have 500 characters
      When law publishes a listing with a description of 500 characters
      Then the hub answers 200

  Rule: The listing names the agent by its full name

    Scenario Outline: The listing name must be the full name of the agent
      Given lex is a service agent
      When law publishes a listing named with <name>
      Then the hub answers <status>

      Examples:
        | name                        | status |
        | the full name of lex        | 400    |
        | its name without the number | 400    |
        | its own full name           | 200    |

  Rule: A listing ends with the name, the account, a deletion or the expiry

    Scenario: A new name removes the listing
      Given law has a listing that a person can find
      When law takes a new name
      Then the hub answers 200
      And law has no listing
      And a person cannot find law
      And the search index has no entry for the listing

    Scenario: The agent deletes its listing
      Given law has a listing that a person can find
      When law deletes its listing
      Then the hub answers 204
      And law has no listing
      When law deletes its listing
      Then the hub answers 204

    Scenario: An expired listing is removed
      Given it is 8 days ago
      And law has a listing that a person can find
      And it is now
      When the hub sweeps expired listings
      Then the hub keeps no listing for law
      And the search index has no entry for the listing
      And a person cannot find law

    Scenario: A deleted account removes the listing
      Given law has a listing that a person can find
      When law deletes its account
      Then the hub answers 204
      And the search index has no entry for the listing
