# Test two nodes on one machine

The host runs Codex. The caller sends it a task. Run the commands from the
repository root.

1. Build the binary:

   ```sh
   cargo build -p aether
   ```

2. Terminal 1, the host:

   ```sh
   target/debug/aether node serve --policy ~/.aether/policy.json  --data-dir ~/.aether/host --port 4242 --admin-port 0
   ```

   If the node prints a `codex login` command, run it once and start again.
3. Terminal 2, the caller:

   ```sh
   target/debug/aether node admin --data-dir ~/.aether/caller --port 0
   ```

4. Open the URL from terminal 1 and the URL from terminal 2 in the browser.
   Accept the certificate warning.
5. Terminal 2 URL: copy the connect code.
6. Terminal 1 URL: paste the code in **Invite someone**, enter a name, and
   select **Create invite**. Copy the `aether-invite:…` line.
7. Terminal 2 URL: paste the line and select **Connect**.
8. Open terminal 3 in the repository root. Keep terminals 1 and 2 running.
   Send a task:

   ```sh
   target/debug/aether node request --data-dir ~/.aether/caller \
     --scope direct --agent-id tudor --address 127.0.0.1:4242 --text 'Give me three ideas for a project name'
   ```

   Use the agent ID that the terminal 2 URL shows.

Terminal 1 starts `codex app-server` by itself, and terminal 3 prints the
answer. The terminal 1 URL shows the task under **Advanced**.
