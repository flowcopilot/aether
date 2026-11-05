Feature: Process commands
  An operator starts the node or the MCP server as a long-running process.
  Each process reports when it is ready, so a supervisor knows it started.

  Scenario Outline: Each process command starts
    When I run `aether <command>`
    Then the command succeeds
    And it reports the "process_ready" event

    Examples:
      | command |
      | node    |
      | mcp     |

  Scenario: An unreadable configuration file is a safe error
    When I run the node with the configuration file "does-not-exist.toml"
    Then the command fails with exit code 2
    And the error is only "aether: configuration file cannot be read"
