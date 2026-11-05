Feature: Blocks
  A person can block another key, so that its invites never reach them.
  The blocked sender cannot tell: the hub answers it as usual. A block also
  ends what the two already share.

  Background:
    Given alice has a name
    And bob has a name

  Scenario: A blocked sender gets the usual answer, and the receiver never sees the invite
    Given carol has a name
    And alice blocked bob
    And alice has read her invites
    When bob invites alice
    And carol invites alice
    Then the answer to bob looks like the answer to carol
    And the invite of bob is pending
    And alice sees only the invite from carol
    And the invite version of alice moved up by one
    And bob sees his invite to alice as its sender

  Scenario Outline: The receiver cannot act on a hidden invite
    Given alice blocked bob
    And bob invited alice
    When alice tries to <action> the invite
    Then the hub answers 404

    Examples:
      | action  |
      | accept  |
      | decline |
      | block   |

  Scenario: The sender of a hidden invite can cancel it
    Given alice blocked bob
    And bob invited alice
    When bob cancels the invite
    Then the hub answers 204
    And bob has no invites

  Scenario: A block removes the open invites and the contact in both directions
    Given alice and bob are contacts
    And bob invited alice
    When alice blocks bob
    Then the hub answers 204
    And alice has no contacts
    And bob has no contacts
    And alice has no invites
    And bob has no invites

  Scenario: The receiver of an open invite blocks its sender by the invite
    Given bob invited alice
    When alice blocks the sender of the invite
    Then the hub answers 204
    And alice has no invites
    And alice has blocked the key of bob
    When bob invites alice
    Then the hub answers 201
    And alice has no invites

  Scenario Outline: Only the receiver of an open invite can block by the invite
    Given bob invited alice
    When <who> tries to block the sender of <invite>
    Then the hub answers 404

    Examples:
      | who          | invite             |
      | bob          | the invite         |
      | a stranger   | the invite         |
      | alice        | an unknown invite  |

  Scenario: A block stays when the blocked account is deleted
    Given alice blocked bob
    When bob deletes his account
    Then alice has blocked a key with no handle
    When bob claims his name again
    And bob invites alice
    Then the hub answers 201
    And alice has no invites

  Scenario: Blocking twice keeps one block, and the block list shows the handle
    When alice blocks bob
    Then the hub answers 204
    When alice blocks bob
    Then the hub answers 204
    And the block list of alice is only bob with his handle

  Scenario: A block can be removed once
    Given alice blocked bob
    When alice unblocks bob
    Then the hub answers 204
    When alice unblocks bob
    Then the hub answers 404

  Scenario Outline: A node cannot block a bad key or itself
    When alice blocks <target>
    Then the hub answers 400

    Examples:
      | target             |
      | a key that is bad  |
      | herself            |

  Scenario: A person has at most 1,000 blocks
    Given alice has 1,000 blocks
    When alice blocks a new account
    Then the hub answers 409 with the error "limit"
