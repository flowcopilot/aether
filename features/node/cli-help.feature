Feature: Command help
  People and agents learn the CLI from its help pages and from its
  machine-readable spec. These pages are a contract, so each change to them
  must get a review.

  Scenario Outline: Each node command explains its flags
    When I ask for help on `aether node <command>`
    Then the help matches the reviewed "help_node_<command>" page

    Examples:
      | command   |
      | admin     |
      | serve     |
      | direct    |
      | invite    |
      | request   |
      | directory |
      | managed   |
      | record    |

  Scenario: The CLI describes itself to agents
    When I ask for the machine-readable spec
    Then the spec matches the reviewed "usage_spec" page
