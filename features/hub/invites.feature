Feature: Invites
  Two people on the hub become contacts through an invite. The sender
  invites the full name of the receiver for some days. The receiver
  accepts, and then the sender sends the signed invite bundle. Each side
  sees the other side's endpoint only when it needs it, and the hub shows
  each change to both sides at once.

  Background:
    Given alice and bob are people

  Rule: An invite goes from pending to accepted to delivered

    Scenario: A new invite is pending
      When alice invites bob for 7 days
      Then the answer is the invite as the sender, pending
      And it is from alice to bob for 7 days
      And it shows when it was made
      And it shows no endpoint
      And it shows no bundle

    Scenario: Both sides see a pending invite
      Given alice invited bob
      Then alice sees the invite as the sender, pending
      And bob sees the invite as the receiver, pending
      And it shows no endpoint

    Scenario: The receiver accepts an invite
      Given alice invited bob
      When bob accepts the invite
      Then the hub answers 200
      And the answer is the invite as the receiver, accepted
      And it shows no endpoint
      And alice sees the invite as the sender, accepted
      And it shows bob's endpoint
      And it shows no bundle

    Scenario: The sender delivers the bundle of an accepted invite
      Given alice invited bob
      And the invite is accepted
      When alice sends the bundle
      Then the hub answers 200
      And the answer is the invite as the sender, delivered
      And it shows bob's endpoint
      And it shows no bundle
      And alice sees the invite as the sender, delivered
      And it shows bob's endpoint
      And bob sees the invite as the receiver, delivered
      And it shows alice's endpoint
      And it shows the bundle

    Scenario: A delivered invite that one side deletes is gone for both
      Given alice invited bob
      And the invite is delivered
      When bob deletes the invite
      Then the hub answers 204
      And alice sees no invites
      And bob sees no invites

  Rule: Either side can end an open invite

    Scenario: The sender cancels an invite
      Given alice invited bob
      When alice deletes the invite
      Then the hub answers 204

    Scenario: The receiver declines an invite
      Given alice invited bob
      When bob deletes the invite
      Then the hub answers 204
      When bob deletes the invite
      Then the hub answers 404

    Scenario: A new invite to the same person replaces the open one
      Given alice invited bob for 7 days
      When alice invites bob for 14 days
      Then the new invite is not the old invite
      And bob sees only the new invite, for 14 days
      When bob accepts the old invite
      Then the hub answers 404

  Rule: Only a person with a name invites another person

    Scenario Outline: The hub checks the receiver and the number of days
      When alice sends an invite to <receiver> for <days>
      Then the hub answers <status>

      Examples:
        | receiver                        | days         | status |
        | herself                         | 7 days       | 400    |
        | an unknown name                 | 7 days       | 404    |
        | bob's name without the number   | 7 days       | 400    |
        | bob's name with the number 0000 | 7 days       | 400    |
        | bob                             | 0 days       | 400    |
        | bob                             | 31 days      | 400    |
        | bob                             | the text "7" | 400    |
        | bob                             | no days      | 400    |
        | bob                             | 30 days      | 201    |
        | bob                             | 1 day        | 201    |

    Scenario: An invite to a service agent looks like an invite to an unknown name
      Given law is a service agent
      When alice sends an invite to law for 7 days
      Then the hub answers 404 with the error "not_found"

    Scenario: A service agent cannot invite by name
      Given law is a service agent
      When law sends an invite to bob for 7 days
      Then the hub answers 403 with the error "forbidden"

    Scenario: A node without a name cannot use invites
      Given carol is a node without a name
      When carol sends an invite to bob for 7 days
      Then the hub answers 403 with the error "no_account"
      When carol lists her invites
      Then the hub answers 403

  Rule: Only the right side moves an invite, in the right order

    Scenario Outline: A step by the wrong side or out of order is forbidden
      Given alice invited bob
      And the invite is <stage>
      When <who> <action>
      Then the hub answers 403 with the error "forbidden"

      Examples:
        | stage     | who   | action             |
        | pending   | alice | accepts the invite |
        | pending   | alice | sends the bundle   |
        | accepted  | bob   | accepts the invite |
        | accepted  | bob   | sends the bundle   |
        | delivered | alice | sends the bundle   |
        | delivered | bob   | accepts the invite |

    Scenario: A person outside the invite does not find it
      Given eve is a person
      And alice invited bob
      When eve accepts the invite
      Then the hub answers 404
      When eve deletes the invite
      Then the hub answers 404

    Scenario: An unknown invite is not found
      When alice accepts an invite that does not exist
      Then the hub answers 404

  Rule: A bundle is an invite line of at most 8 KiB

    Scenario Outline: The hub checks the bundle
      Given alice invited bob
      And the invite is accepted
      When alice sends <bundle>
      Then the hub answers <status>

      Examples:
        | bundle                                       | status |
        | a bundle without the invite prefix           | 400    |
        | a bundle with 8 KiB of text after the prefix | 400    |
        | no bundle                                    | 400    |
        | a bundle of exactly 8 KiB with the prefix    | 200    |

  Rule: Each side's invite list has a version that rises on each change

    Scenario: Each change raises the version for both sides
      Given the versions of alice and bob are noted
      When alice invites bob for 7 days
      Then the versions of alice and bob are 1 higher
      When bob accepts the invite
      Then the versions of alice and bob are 2 higher

    Scenario: A new name shows in open invites and raises the other side's version
      Given alice invited bob
      And the version of bob is noted
      When alice takes a new name
      Then the hub answers 200
      And bob sees one invite, from alice
      And the version of bob is 1 higher

    Scenario: An invite expires after 7 days and the sweep removes it
      Given the version of bob is noted
      And it is 7 days ago
      And alice invited bob
      And it is now
      Then bob sees no invites
      When bob accepts the invite
      Then the hub answers 404
      When the hub sweeps expired invites
      Then the hub keeps no record of the invite
      And the version of bob is 2 higher

  Rule: A node can wait for the next change of its invite list

    Scenario: A wait ends early when a change arrives
      Given the version of bob is noted
      When bob starts to wait up to 10 seconds for a change after that version
      And alice invites bob for 7 days
      Then the wait ends with the version 1 higher and the new invite for bob as receiver
      And the wait took less than 5 seconds

    Scenario: A wait without a change ends after its time with the same version
      Given the version of bob is noted
      When bob waits up to 1 second for a change after that version
      Then the wait ends with the same version and no invites
      And the wait took at least 1 second

    Scenario: A wait for an older version ends at once
      Given the version of bob is noted
      When bob waits up to 10 seconds for a change after an older version
      Then the wait ends with the noted version
      And the wait took less than 1 second

    Scenario Outline: A wait with a parameter out of range is refused
      When bob lists his invites with <parameter>
      Then the hub answers 400

      Examples:
        | parameter |
        | wait=31   |
        | after=x   |
        | wait=-1   |
