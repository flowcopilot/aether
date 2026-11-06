Feature: Contacts
  Two nodes become contacts after one accepts an invite of the other and
  the sender delivers its signed bundle. Each node keeps a list of its
  contacts on the hub, with their current handles, so that it can find them
  again and see if they are online.

  Background:
    Given alice has a name
    And bob has a name

  Scenario: A delivered invite makes a contact pair that stays after the invite is deleted
    Given alice and bob are contacts
    When bob deletes the invite
    Then the hub answers 204
    And alice has only bob as a contact
    And bob has only alice as a contact

  Scenario: A pending or accepted invite makes no contact
    When alice invites bob
    Then alice has no contacts
    When bob accepts the invite
    Then alice has no contacts
    And bob has no contacts

  Scenario: A second delivery between the same nodes keeps one pair
    Given alice and bob are contacts
    When alice and bob become contacts again with a new invite
    Then alice has 1 contact
    And bob has 1 contact

  Scenario: A node sees only its own contacts, in the order of their handles
    Given dave and erin have names, and the handle of dave sorts first
    And carol has a name
    And alice and erin are contacts
    And dave and alice are contacts
    And bob and carol are contacts
    Then alice sees her contacts in this order: dave, erin

  Scenario: A contact shows the current handle
    Given alice and bob are contacts
    When bob changes his name
    Then alice sees the new handle of bob, as a personal agent

  Scenario: Removing a contact removes it for both nodes
    Given alice and bob are contacts
    When alice removes bob from her contacts
    Then the hub answers 204
    And alice has no contacts
    And bob has no contacts
    When alice removes bob from her contacts
    Then the hub answers 404 with the error "not_found"

  Scenario: A node cannot remove a contact with a bad key
    Given alice has a name
    When alice removes a contact with a bad key
    Then the hub answers 400

  Scenario: A node without an account has no contact list
    Given zed is a new node
    When zed reads his contacts
    Then the hub answers with the error "no_account"

  Scenario: A node without an account cannot remove a contact
    Given zed is a new node
    When zed removes bob from his contacts
    Then the hub answers 403

  Rule: A contact shows if the other node is online, and when it was last seen
    A node is online when its last signed request was at most 45 seconds
    ago, or when it waits for news in an open long poll. The last-seen time
    is rounded down to 15 minutes, so that contacts cannot follow each
    other closely.

    Scenario Outline: A node seen in the last 45 seconds is online
      Given alice and bob are contacts
      When bob was last seen <seconds> seconds ago
      Then alice sees bob <presence>, last seen <seconds> seconds ago rounded down to 15 minutes

      Examples:
        | seconds | presence |
        | 45      | online   |
        | 46      | offline  |

    Scenario: A signed request marks the sender as seen
      Given alice and bob are contacts
      And bob was last seen long ago
      When bob sends a signed request
      Then alice sees bob online, last seen now rounded down to 15 minutes

    Scenario: The last-seen time is rounded down to 15 minutes
      Given alice and bob are contacts
      When bob was last seen one hour ago, in the last second of a 15-minute block
      Then alice sees bob offline, last seen at the start of that block

    Scenario: An open long poll counts as online
      Given alice and bob are contacts
      And bob was last seen 600 seconds ago
      When bob waits for news for 2 seconds
      Then alice sees bob online
      When the wait of bob ends
      Then alice sees bob offline

    Scenario: A change in presence does not wake the other node
      Given alice and bob are contacts
      And alice has read her invites
      When bob sends a signed request
      Then the invite version of alice did not move

    Scenario: A contact not seen since the hub started has no last-seen time
      Given alice has a contact ghost that the hub has not seen since it started
      Then alice sees only ghost, offline and with no last-seen time

    Scenario: The hub forgets presence after 7 days
      Given one node was last seen 7 days and 1 second ago
      And another node was last seen 7 days less 1 minute ago
      When the hub cleans up presence
      Then the hub forgets the first node
      And the hub still knows when it saw the second node
