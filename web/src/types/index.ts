export interface ClientRow {
  tenant_id: string
  client_id: string
  run_id: string
  connected_at_ms: number
  proxy_count: number
}

export interface ProxyProbeResult {
  name: string
  supported: boolean
  ok: boolean
  latency_ms: number
  error_code: string
  message: string
  error?: string
}

export interface ProxyRow {
  name: string
  type: string
  remote_port: number
  domains: string[]
  tenant_id: string
  client_id: string
  run_id: string
  local_addr: string
  rate_limit_bps: number
  max_conns: number
  idle_timeout_s: number
  allow_cidrs: string[]
  active_conns: number
  conn_total: number
  bytes_in: number
  bytes_out: number
  local_connect_fail: number
}

export interface StatusTotals {
  clients: number
  proxies: number
  active_conns: number
  bytes_in: number
  bytes_out: number
}

export interface StatusSnapshot {
  status: string
  uptime_s: number
  clients: ClientRow[]
  proxies: ProxyRow[]
  totals: StatusTotals
}

export interface SettingsSnapshot {
  server: {
    bind: string
    control_port: number
    tls_cert: string | null
    tls_configured: boolean
  }
  admin: {
    bind: string
    panel: boolean
    loopback: boolean
    token_configured: boolean
  }
  http: {
    enabled: boolean
    bind: string
    https_bind: string
    domain: string
  }
  acme: {
    enabled: boolean
    challenge: string
    staging: boolean
    cert_dir: string
    email: string | null
    email_configured: boolean
    dns_provider: string | null
    dns_token_configured: boolean
  }
  socks5: {
    enabled: boolean
    bind: string
  }
  log: {
    level: string
    format: string
  }
  tenant: {
    default: string
  }
  ports: {
    range: [number, number]
  }
  session: {
    ttl_s: number
    cookie_secure: boolean
    csrf_required: boolean
    login_max_failures: number
    login_lock_s: number
  }
  writes: {
    methods: string[]
    proxy_crud: boolean
    settings: boolean
    config_path: string | null
    config_format?: 'toml' | 'yaml' | 'auto' | null
  }
  reload: {
    supported: boolean
    config_version: string
    last_reload_ms: number | null
    reload_count: number
    last_error: string | null
    restart_required: string[]
  }
}

export interface SettingsUpdateResponse {
  ok: boolean
  restart_required: boolean
  restart_delay_s: number
  applied?: boolean
  config_version?: string
  last_reload_ms?: number | null
  restart_fields?: string[]
  config_path: string
  config_format?: 'toml' | 'yaml' | 'auto'
  backup: string | null
  message: string
}

export interface CertificateEntry {
  domain: string
  proxy: string
  type: string
  remote_port: number
  source: string
  challenge: string
  status: string
  acme_state: string | null
  error: string | null
  last_attempt: number | null
  not_after: number | null
  days_left: number | null
  serial: string | null
  issuer: string | null
  chain_length: number
}

export interface CertificatesSnapshot {
  acme_enabled: boolean
  staging: boolean
  cert_dir: string
  cached_count: number
  entries: CertificateEntry[]
}

export interface ProxyWriteResponse {
  ok: boolean
  name: string
  remote_port?: number
  domains?: string[]
}

export interface LogsSnapshot {
  lines: string[]
  total: number
}

export interface SessionSnapshot {
  authenticated: boolean
  csrf: string | null
  mode: 'loopback' | 'session'
}
