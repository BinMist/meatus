export interface CertificateRowView {
  domain: string
  proxy: string
  issuerText: string
  status: string
  tagStatus: 'default' | 'success' | 'processing' | 'warning' | 'error'
  tagLabel: string
  type: string
  sourceText: string
  chainText: string
  expiresText: string
  daysText: string
  daysTone: 'ok' | 'warn' | 'danger'
  serialText: string
  [key: string]: unknown
}
