export interface AuditEvent {
  ts_ms: number
  actor: string
  source_ip: string
  action: string
  target: string
  client_id?: string | null
  result: string
  trace_id: string
}

export interface AuditSnapshot {
  events: AuditEvent[]
  persistent: boolean
  dir: string | null
}
