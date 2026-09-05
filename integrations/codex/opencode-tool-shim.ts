// Only adapts OpenCode's registration API. The vendored execute function is unchanged.
export class StringArgument {
  description = ""
  isOptional = false
  describe(description: string) { this.description = description; return this }
  optional() { this.isOptional = true; return this }
}

export function tool<T>(definition: T): T { return definition }
tool.schema = { string: () => new StringArgument() }
