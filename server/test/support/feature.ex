defmodule AetherHub.Feature do
  @moduledoc """
  Runs a Gherkin file from `features/hub/` at the repository root as ExUnit
  tests: one test for each scenario, and one for each example row of an
  outline.

      defmodule AetherHub.Features.InvitesTest do
        use AetherHub.Feature, file: "invites.feature", async: true

        defstep ~r/^"(?<name>\\w+)" invites "(?<other>\\w+)"$/, %{name: name}, state do
          {:ok, %{invite: ...}}
        end
      end

  A step definition matches the step text without its keyword, so one
  definition serves `Given`, `When`, `Then`, `And` and `But`. The second
  argument matches the named captures (as atom keys), with `:table` (the
  rows of a data table as maps from the header) and `:doc_string`. The
  third argument matches the scenario state, which starts as the ExUnit
  context. A step that returns `{:ok, map}` merges the map into the state;
  any other value leaves the state as it was.

  Scenario tags become ExUnit tags, so `mix test --only slow` selects the
  scenarios tagged `@slow`.
  """

  @features Path.expand("../../../features/hub", __DIR__)

  @doc "The directory of the hub's feature files."
  def features_dir, do: @features

  defmacro __using__(opts) do
    file = Keyword.fetch!(opts, :file)
    async = Keyword.get(opts, :async, false)

    # The scenarios become tests in this module's `__before_compile__`,
    # which must run before ExUnit's own collects the tests.
    quote do
      @before_compile AetherHub.Feature
      use ExUnit.Case, async: unquote(async)
      import AetherHub.Feature, only: [defstep: 4]

      Module.register_attribute(__MODULE__, :feature_steps, accumulate: true)
      @feature_file Path.join(AetherHub.Feature.features_dir(), unquote(file))
      @external_resource @feature_file
    end
  end

  @doc "Defines a step. See the module documentation."
  defmacro defstep(regex, vars, state, do: block) do
    {%Regex{} = compiled, _} = Code.eval_quoted(regex, [], __CALLER__)
    # Named from the pattern, so steps from a shared module do not clash.
    name = :"__step_#{:erlang.phash2({Regex.source(compiled), Regex.opts(compiled)})}__"
    location = "#{Path.relative_to_cwd(__CALLER__.file)}:#{__CALLER__.line}"

    quote do
      @feature_steps {unquote(name), unquote(Regex.source(compiled)),
                      unquote(Regex.opts(compiled)), unquote(location)}
      def unquote(name)(unquote(vars), unquote(state)), do: unquote(block)
    end
  end

  defmacro __before_compile__(env) do
    file = Module.get_attribute(env.module, :feature_file)
    steps = Module.get_attribute(env.module, :feature_steps) |> Enum.reverse()

    for scenario <- file |> File.read!() |> parse(file) do
      tags = Enum.map(scenario.tags, &String.to_atom/1)
      steps_ast = Macro.escape(scenario.steps)

      quote do
        @tag unquote(tags |> Enum.map(&{&1, true}))
        test unquote(scenario.name), context do
          AetherHub.Feature.run(
            __MODULE__,
            unquote(Macro.escape(steps)),
            unquote(steps_ast),
            Map.new(context)
          )
        end
      end
    end
  end

  @doc false
  def run(module, definitions, steps, state) do
    Enum.reduce(steps, state, fn step, state ->
      {name, vars} = find(definitions, step)

      case apply(module, name, [vars, state]) do
        {:ok, %{} = changes} -> Map.merge(state, changes)
        _ -> state
      end
    end)
  end

  defp find(definitions, step) do
    matches =
      for {name, source, opts, location} <- definitions,
          regex = Regex.compile!(source, opts),
          captures = Regex.named_captures(regex, step.text),
          captures != nil do
        vars =
          captures
          |> Map.new(fn {key, value} -> {String.to_atom(key), value} end)
          |> Map.merge(%{table: table(step.table), doc_string: step.doc_string})

        {name, vars, location}
      end

    case matches do
      [{name, vars, _}] ->
        {name, vars}

      [] ->
        raise ExUnit.AssertionError,
          message: "no step definition matches line #{step.line}: #{step.keyword} #{step.text}"

      many ->
        raise ExUnit.AssertionError,
          message:
            "more than one step definition matches line #{step.line}: #{step.text}\n" <>
              Enum.map_join(many, "\n", fn {_, _, location} -> "  " <> location end)
    end
  end

  defp table([]), do: []
  defp table([header | rows]), do: Enum.map(rows, &(header |> Enum.zip(&1) |> Map.new()))

  # The parser. It reads the parts of Gherkin that the hub's features use:
  # Feature, Rule, Background, Scenario (Example), Scenario Outline
  # (Scenario Template) with Examples (Scenarios), the step keywords, data
  # tables, doc strings, tags and comments. Other text is description.

  @step_keywords ~w(Given When Then And But *)
  @block_keywords [
    "Rule",
    "Background",
    "Scenario Outline",
    "Scenario Template",
    "Scenario",
    "Example"
  ]

  @doc false
  def parse(text, file) do
    initial = %{
      file: file,
      tags: [],
      feature_tags: [],
      background: [],
      rule: nil,
      current: nil,
      scenarios: [],
      doc: nil
    }

    text
    |> String.split(~r/\r?\n/)
    |> Enum.with_index(1)
    |> Enum.reduce(initial, &line/2)
    |> close()
    |> Map.fetch!(:scenarios)
    |> Enum.reverse()
    |> Enum.flat_map(&expand/1)
    |> check_unique(file)
  end

  # Inside a doc string, lines are kept until the closing delimiter.
  defp line({raw, _number}, %{doc: {delimiter, indent, lines}} = parser) do
    if String.trim(raw) == delimiter do
      text = lines |> Enum.reverse() |> Enum.map_join("\n", &dedent(&1, indent))
      update_last_step(%{parser | doc: nil}, &%{&1 | doc_string: text})
    else
      %{parser | doc: {delimiter, indent, [raw | lines]}}
    end
  end

  defp line({raw, number}, parser) do
    text = String.trim(raw)

    cond do
      text == "" or String.starts_with?(text, "#") ->
        parser

      # The free text under Feature and Rule is description, even when a
      # line starts with a step keyword.
      parser.current == :description and not String.starts_with?(text, "@") and
          not keyword?(text, @block_keywords) ->
        parser

      String.starts_with?(text, "@") ->
        %{parser | tags: parser.tags ++ tags(text)}

      String.starts_with?(text, ~w(""" ```)) ->
        delimiter = String.slice(text, 0, 3)
        indent = String.length(raw) - String.length(String.trim_leading(raw))
        %{parser | doc: {delimiter, indent, []}}

      String.starts_with?(text, "|") ->
        row = cells(text)

        case parser.current do
          %{examples: [_ | _]} -> update_examples(parser, row)
          _ -> update_last_step(parser, &%{&1 | table: &1.table ++ [row]})
        end

      keyword?(text, ["Feature"]) ->
        %{parser | feature_tags: parser.tags, tags: [], current: :description}

      keyword?(text, ["Rule"]) ->
        parser = close(parser)
        %{parser | rule: %{tags: parser.tags, background: []}, tags: [], current: :description}

      keyword?(text, ["Background"]) ->
        parser = close(parser)
        %{parser | current: :background, tags: []}

      keyword?(text, ["Scenario Outline", "Scenario Template"]) ->
        start_scenario(parser, text, number, true)

      keyword?(text, ["Scenario", "Example"]) ->
        start_scenario(parser, text, number, false)

      keyword?(text, ["Examples", "Scenarios"]) ->
        update_in(parser.current.examples, &(&1 ++ [%{tags: parser.tags, rows: []}]))
        |> Map.put(:tags, [])

      step = step(text, number) ->
        add_step(parser, step)

      true ->
        parser
    end
  end

  defp keyword?(text, keywords), do: Enum.any?(keywords, &String.starts_with?(text, &1 <> ":"))

  defp title(text), do: text |> String.split(":", parts: 2) |> List.last() |> String.trim()

  defp tags(text) do
    text
    |> String.split(~r/\s+#/, parts: 2)
    |> hd()
    |> String.split()
    |> Enum.map(&String.trim_leading(&1, "@"))
  end

  defp cells(text) do
    text
    |> String.trim()
    |> String.trim_leading("|")
    |> String.trim_trailing("|")
    |> String.split(~r/(?<!\\)\|/)
    |> Enum.map(&(&1 |> String.trim() |> String.replace("\\|", "|")))
  end

  defp step(text, number) do
    Enum.find_value(@step_keywords, fn keyword ->
      if String.starts_with?(text, keyword <> " ") do
        %{
          keyword: keyword,
          text: text |> String.replace_prefix(keyword <> " ", "") |> String.trim(),
          table: [],
          doc_string: "",
          line: number
        }
      end
    end)
  end

  defp start_scenario(parser, text, number, outline?) do
    parser = close(parser)
    rule_tags = if parser.rule, do: parser.rule.tags, else: []

    scenario = %{
      name: title(text),
      line: number,
      tags: Enum.uniq(parser.feature_tags ++ rule_tags ++ parser.tags),
      background: parser.background ++ if(parser.rule, do: parser.rule.background, else: []),
      steps: [],
      outline?: outline?,
      examples: []
    }

    %{parser | current: scenario, tags: []}
  end

  defp add_step(%{current: :background, rule: nil} = parser, step),
    do: %{parser | background: parser.background ++ [step]}

  defp add_step(%{current: :background} = parser, step),
    do: update_in(parser.rule.background, &(&1 ++ [step]))

  defp add_step(%{current: %{}} = parser, step),
    do: update_in(parser.current.steps, &(&1 ++ [step]))

  defp add_step(parser, step),
    do: raise("#{parser.file}:#{step.line}: a step outside a scenario or background")

  defp update_last_step(%{current: :background, rule: nil} = parser, fun),
    do: %{parser | background: List.update_at(parser.background, -1, fun)}

  defp update_last_step(%{current: :background} = parser, fun),
    do: update_in(parser.rule.background, &List.update_at(&1, -1, fun))

  defp update_last_step(parser, fun),
    do: update_in(parser.current.steps, &List.update_at(&1, -1, fun))

  defp update_examples(parser, row) do
    update_in(parser.current.examples, fn examples ->
      List.update_at(examples, -1, &%{&1 | rows: &1.rows ++ [row]})
    end)
  end

  defp close(%{current: %{} = scenario} = parser),
    do: %{parser | current: nil, scenarios: [scenario | parser.scenarios]}

  defp close(parser), do: %{parser | current: nil}

  defp dedent(line, indent) do
    {lead, rest} = String.split_at(line, indent)
    if String.trim(lead) == "", do: rest, else: line
  end

  defp expand(%{outline?: false} = scenario),
    do: [
      %{name: scenario.name, tags: scenario.tags, steps: scenario.background ++ scenario.steps}
    ]

  defp expand(%{outline?: true} = scenario) do
    for %{tags: tags, rows: [header | rows]} <- scenario.examples, row <- rows do
      values = Enum.zip(header, row)

      fill = fn text ->
        Enum.reduce(values, text, fn {k, v}, t -> String.replace(t, "<#{k}>", v) end)
      end

      steps =
        Enum.map(scenario.steps, fn step ->
          %{
            step
            | text: fill.(step.text),
              doc_string: fill.(step.doc_string),
              table: Enum.map(step.table, fn cells -> Enum.map(cells, fill) end)
          }
        end)

      %{
        name: "#{scenario.name} [#{Enum.map_join(values, ", ", fn {k, v} -> "#{k}=#{v}" end)}]",
        tags: Enum.uniq(scenario.tags ++ tags),
        steps: scenario.background ++ steps
      }
    end
  end

  defp check_unique(scenarios, file) do
    case scenarios |> Enum.frequencies_by(& &1.name) |> Enum.filter(fn {_, n} -> n > 1 end) do
      [] -> scenarios
      [{name, _} | _] -> raise "#{file}: two scenarios are named #{inspect(name)}"
    end
  end
end
