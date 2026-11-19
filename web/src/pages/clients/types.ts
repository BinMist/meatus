export interface ClientRow {
  id: string
  tenant: string
  token_fingerprint: string
  allow_ports: [number, number] | null
  source: 'clients' | 'token_sha256'
  online: boolean
  run_count: number
  proxy_count: number
  last_connected_ms: number
  can_delete: boolean
}

export interface ClientsSnapshot {
  clients: ClientRow[]
  server_addr: string
  server_name: string
  config_path: string | null
  config_format: string | null
  restart_required: boolean
  restart_delay_s: number
}

export interface ClientConfigSnippets {
  toml: string
  yaml: string
  format: string
}

export interface ClientWriteResponse {
  ok: boolean
  client?: ClientRow
  id?: string
  token?: string
  config?: ClientConfigSnippets
  restart_required: boolean
  restart_delay_s: number
  backup?: string | null
  message?: string
}
