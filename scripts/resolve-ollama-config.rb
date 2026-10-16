#!/usr/bin/env ruby
# Resolve Ollama launch inputs from Magician's operation mappings and profiles.

require "yaml"
require "uri"

path = ARGV.fetch(0) { abort "usage: resolve-ollama-config.rb CONFIG_PATH" }
config = YAML.safe_load(File.read(path), aliases: true) || {}
runtime = config.dig("runtime", "ollama") || {}
router = config.dig("llm", "router") || {}
profiles = router["profiles"] || {}
adaptive_profiles = router["adaptive_profiles"] || {}
mappings = router["operation_mapping"] || {}
# Operator locality. Under `cloud`, a conditional selector's `when_cloud` arm is
# the profile that serves the operation (mirrors magicllm's
# `OperationProfileSelector::profile_for_locality`), so the local `default` arm
# must not count as a mapped generation model.
cloud_locality = config.dig("privacy", "processing", "mode").to_s.strip.casecmp?("cloud")

required_runtime = %w[
  keep_alive
  max_loaded_models
  embedding_base_url
  embedding_keep_alive
  embedding_num_parallel
  embedding_max_loaded_models
  embedding_query_timeout_ms
  embedding_write_timeout_ms
  embedding_context_tokens
  embedding_batch_tokens
  embedding_batch_size
  embedding_model
  embedding_dimensions
  kv_cache_type
  flash_attention
  prewarm
  replace_existing_local_daemon
]
required_runtime.each do |key|
  value = runtime[key]
  abort "#{path}: runtime.ollama.#{key} is required" if value.nil? || value.to_s.strip.empty?
end

embedding_url = runtime.fetch("embedding_base_url").to_s.strip
abort "#{path}: runtime.ollama.embedding_base_url must be an http(s) URL" unless embedding_url.match?(%r{\Ahttps?://})
abort "#{path}: runtime.ollama.embedding_keep_alive must be -1" unless runtime.fetch("embedding_keep_alive").to_s.strip == "-1"

embedding_parallel = Integer(runtime.fetch("embedding_num_parallel"), exception: false)
abort "#{path}: runtime.ollama.embedding_num_parallel must be 1 until multi-sequence embedding execution is verified" unless embedding_parallel == 1
embedding_models = Integer(runtime.fetch("embedding_max_loaded_models"), exception: false)
abort "#{path}: runtime.ollama.embedding_max_loaded_models must be 1" unless embedding_models == 1
embedding_query_timeout = Integer(runtime.fetch("embedding_query_timeout_ms"), exception: false)
abort "#{path}: runtime.ollama.embedding_query_timeout_ms must be between 100 and 60000" unless embedding_query_timeout&.between?(100, 60_000)
embedding_write_timeout = Integer(runtime.fetch("embedding_write_timeout_ms"), exception: false)
abort "#{path}: runtime.ollama.embedding_write_timeout_ms must be between 1000 and 900000" unless embedding_write_timeout&.between?(1_000, 900_000)

%w[embedding_context_tokens embedding_batch_tokens embedding_batch_size embedding_dimensions].each do |key|
  value = Integer(runtime.fetch(key), exception: false)
  abort "#{path}: runtime.ollama.#{key} must be a positive integer" unless value&.positive?
end

# Load-time rule mirrored from magician's config normalization: cloud locality
# disables local pre-summarisation and unmaps its operation, so its local model
# must not stay resident for an operation that never runs.
mappings = mappings.reject { |operation, _| operation == "local_prep" } if cloud_locality

mapped_names = mappings.values.flat_map do |selector|
  case selector
  when String
    [selector]
  when Hash
    if cloud_locality && selector["when_cloud"]
      [selector["when_cloud"]]
    else
      [selector["default"], selector["when_has_images"]].compact
    end
  else
    abort "#{path}: unsupported operation mapping selector #{selector.inspect}"
  end
end.uniq

concrete_names = mapped_names.flat_map do |name|
  if profiles.key?(name)
    [name]
  elsif adaptive_profiles.key?(name)
    adaptive = adaptive_profiles.fetch(name)
    [adaptive["fast_profile"], adaptive["thinking_profile"]]
  else
    abort "#{path}: operation mapping references missing profile #{name.inspect}"
  end
end.uniq

listener_key = lambda do |endpoint|
  uri = URI.parse(endpoint.to_s)
  host = uri.host.to_s.downcase
  if host.empty?
    nil
  else
    host = "loopback" if %w[localhost 127.0.0.1 ::1].include?(host)
    [host, uri.port]
  end
rescue URI::InvalidURIError
  nil
end

embedding_listener = listener_key.call(runtime.fetch("embedding_base_url"))
# The embedding profile is mapped like any other operation and is an Ollama
# profile on the embedding listener by design. Every check below is about
# generation, so it is identified once here and skipped throughout; without
# this the listener guard reports the embedding daemon colliding with itself.
embedding_model_name = runtime.fetch("embedding_model").to_s.strip
generation_profile = lambda do |profile|
  profile["provider"].to_s == "ollama" && profile["model"].to_s.strip != embedding_model_name
end
concrete_names.each do |name|
  profile = profiles.fetch(name)
  next unless generation_profile.call(profile)

  generation_listener = listener_key.call(profile["api_base_url"])
  abort "#{path}: Ollama profile #{name.inspect} must set a valid api_base_url" unless generation_listener
  if generation_listener == embedding_listener
    abort "#{path}: runtime.ollama.embedding_base_url must use a different listener from Ollama generation profile #{name.inspect}"
  end
end

contexts_by_model = {}
concrete_names.each do |name|
  profile = profiles.fetch(name) { abort "#{path}: missing concrete profile #{name.inspect}" }
  next unless generation_profile.call(profile)

  model = profile["model"].to_s.strip
  abort "#{path}: Ollama profile #{name.inspect} has no model" if model.empty?
  context = profile.dig("metadata", "options", "num_ctx")
  context = Integer(context, exception: false)
  abort "#{path}: Ollama profile #{name.inspect} must set metadata.options.num_ctx" unless context&.positive?
  contexts_by_model[model] = [contexts_by_model.fetch(model, 0), context].max
end

mappings.sort.each do |operation, selector|
  profile_name = selector.is_a?(Hash) ? ((cloud_locality && selector["when_cloud"]) || selector["default"]) : selector
  if adaptive_profiles.key?(profile_name)
    profile_name = adaptive_profiles.fetch(profile_name).fetch("fast_profile")
  end
  profile = profiles.fetch(profile_name) { abort "#{path}: missing profile #{profile_name.inspect}" }
  next unless generation_profile.call(profile)

  model = profile["model"].to_s.strip
  context = Integer(profile.dig("metadata", "options", "num_ctx"), exception: false)
  endpoint = profile["api_base_url"].to_s.strip
  abort "#{path}: Ollama profile #{profile_name.inspect} has no model" if model.empty?
  abort "#{path}: Ollama profile #{profile_name.inspect} must set metadata.options.num_ctx" unless context&.positive?
  abort "#{path}: Ollama profile #{profile_name.inspect} must set api_base_url" if endpoint.empty?
  puts "operation_#{operation}_model=#{model}"
  puts "operation_#{operation}_context_tokens=#{context}"
  puts "operation_#{operation}_endpoint=#{endpoint}"
end

daemon_context = contexts_by_model.values.max
if daemon_context.nil?
  # Cloud locality maps every local-eligible operation to its remote arm, so no
  # generation model is mapped and the daemon only hosts the embedder.
  abort "#{path}: at least one mapped Ollama generation model is required" unless cloud_locality
  daemon_context = Integer(runtime.fetch("embedding_context_tokens"))
end

# Local generation gate and tiers. Emitted as flat keys so the installer can
# read them with the same awk it uses for everything else, and so the gate is
# stated in exactly one place — this config — rather than duplicated in shell.
local_generation = runtime["local_generation"]
if local_generation.is_a?(Hash)
  puts "local_generation_min_memory_gb=#{local_generation.fetch("min_memory_gb", 0)}"
  puts "local_generation_requires_arch=#{local_generation.fetch("requires_arch", "")}"
  puts "local_generation_requires_os=#{local_generation.fetch("requires_os", "")}"
  puts "local_generation_selected=#{local_generation.fetch("selected", "")}"
  puts "local_generation_embedding_resident_gb=#{local_generation.fetch("embedding_resident_gb", 0)}"
  puts "local_generation_system_headroom_gb=#{local_generation.fetch("system_headroom_gb", 0)}"
  tiers = local_generation.fetch("tiers", [])
  puts "local_generation_tier_count=#{tiers.length}"
  tiers.each_with_index do |tier, index|
    puts "local_generation_tier_#{index}_min_memory_gb=#{tier.fetch("min_memory_gb", 0)}"
    puts "local_generation_tier_#{index}_model=#{tier.fetch("model", "")}"
    puts "local_generation_tier_#{index}_resident_gb=#{tier.fetch("resident_gb", 0)}"
  end
else
  puts "local_generation_tier_count=0"
end

puts "max_loaded_models=#{runtime.fetch("max_loaded_models")}"
puts "daemon_context_tokens=#{daemon_context}"
puts "generation_model_count=#{contexts_by_model.length}"
contexts_by_model.sort.each_with_index do |(model, context), index|
  puts "generation_model_#{index}=#{model}"
  puts "generation_context_tokens_#{index}=#{context}"
end
%w[
  embedding_context_tokens
  embedding_base_url
  embedding_keep_alive
  embedding_num_parallel
  embedding_max_loaded_models
  embedding_query_timeout_ms
  embedding_write_timeout_ms
  embedding_batch_tokens
  embedding_batch_size
  embedding_model
  embedding_dimensions
  kv_cache_type
  flash_attention
  prewarm
  replace_existing_local_daemon
].each { |key| puts "#{key}=#{runtime.fetch(key)}" }
puts "keep_alive=#{runtime["keep_alive"]}"
