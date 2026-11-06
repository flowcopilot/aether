defmodule AetherHub.ListingValidationVectorsTest do
  use ExUnit.Case, async: true

  alias AetherHub.{Contract, Listings}

  test "listing validation vectors match protocol/hub/vectors.json" do
    for vector <- Contract.vectors()["listingValidation"] do
      bytes = vector["listingBytes"]
      handle = vector["handle"]
      valid = vector["valid"]
      assert Listings.valid_listing?(bytes, handle) == valid, vector["name"]
    end
  end
end
