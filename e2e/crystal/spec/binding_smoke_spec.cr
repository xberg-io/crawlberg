require "./spec_helper"

describe Crawlberg do
  it "links the generated binding" do
    Crawlberg::VERSION.should_not be_empty
  end
end
