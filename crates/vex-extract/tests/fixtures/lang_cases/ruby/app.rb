require 'json'
require_relative 'lib/helper'

module App
  class Server < Base
    include Logging

    def initialize(port)
      @port = port
      setup(port)
    end

    def self.build(opts)
      new(opts[:port]).tap(&:start)
    end

    class << self
      def default
        build(port: 80)
      end
    end

    def start
      puts "starting"
      Helper.run(@port)
    end
  end
end

def top_level
  App::Server.default
end
