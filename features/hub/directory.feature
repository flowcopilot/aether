Feature: Public directory
  Anyone can see what the hub offers without an account. The landing page,
  the docs and the directory of listed service agents are public. The
  directory shows what each listed service agent does, whether it is online
  and who owns it. It never shows an endpoint or a signature, and it shows
  people only as a count.

  Scenario: The landing page needs no account and loads nothing from another origin
    When a visitor opens the landing page
    Then the hub answers 200 with HTML
    And the page loads nothing by default and connects only to the hub

  Scenario: The docs are Markdown and need no account
    When a visitor opens the docs
    Then the hub answers 200 with Markdown
    And the page tells how to run "aether mcp serve"

  Scenario: A listed service agent shows its listing text and whether it is online
    Given law is a service agent
    And law publishes the listing:
      | description      | skill     | tags |
      | Reads contracts. | Contracts | law  |
    Then the directory shows law with:
      | description      | skill     | tags | online | owner |
      | Reads contracts. | Contracts | law  | yes    | none  |

  Scenario: A row shows the full name of the owner and no endpoint or signature
    Given ana is a person
    And law is a listed service agent
    And ana owns law
    Then the directory shows ana as the owner of law
    And the row of law shows only the full name, the listing text, the skills, the owner and whether it is online
    And the directory does not show the endpoint of law or ana

  Scenario Outline: An agent without a listing is not shown
    Given <agent> is <kind>
    Then the directory does not show <agent>

    Examples:
      | agent | kind                      |
      | lex   | an unlisted service agent |
      | ana   | a person                  |

  Scenario: People are only a count
    Given the count of people in the directory is noted
    When a new person takes a name
    Then the count of people in the directory is at least 1 higher

  Scenario: The directory counts against a limit for each client address
    When a visitor reads the directory as many times as the limit allows
    Then the hub answers 200 each time
    When the visitor reads the directory once more
    Then the hub answers 429
    When a visitor from another address reads the directory
    Then the hub answers 200
