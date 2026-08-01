import request from '../request'

export interface BackupSettingsView {
  storageRevision: number
  endpoint: string | null
  region: string | null
  bucket: string | null
  accessKeyId: string | null
  secretAccessKey: string | null
  prefix: string | null
  forcePathStyle: boolean
  verified: boolean
  scheduleEnabled: boolean
  cronExpression: string | null
  scheduleTimezone: string | null
  retentionDays: number
  retentionCount: number
  nextRunAt: string | null
  lastVerifiedAt: string | null
  updatedAt: string
}

export interface UpdateBackupStoragePayload {
  endpoint: string
  region: string
  bucket: string
  accessKeyId: string
  secretAccessKey?: string
  prefix: string
  forcePathStyle: boolean
}

export interface UpdateBackupSchedulePayload {
  scheduleEnabled: boolean
  cronExpression: string
  scheduleTimezone: string
  retentionDays: number
  retentionCount: number
}

export interface ConnectionTestResult {
  ok: boolean
  stage: string
  code: string | null
  message: string
}

export type BackupTriggerKind = 'manual' | 'scheduled'
export type BackupStatus
  = | 'queued'
    | 'dumping'
    | 'uploading'
    | 'completed'
    | 'failed'
    | 'deleting'

export interface BackupRecord {
  id: string
  triggerKind: BackupTriggerKind
  status: BackupStatus
  scheduledAt: string | null
  objectKey: string
  sizeBytes: number | null
  sha256: string | null
  attemptCount: number
  errorCode: string | null
  errorMessage: string | null
  startedAt: string | null
  completedAt: string | null
  expiresAt: string | null
  createdAt: string
  updatedAt: string
}

export interface BackupRecordPage {
  items: BackupRecord[]
  page: {
    page: number
    pageSize: number
    total: number
    totalPages: number
  }
}

export interface BackupRecordsParams {
  page: number
  pageSize: number
  status?: BackupStatus
  trigger?: BackupTriggerKind
}

export interface DownloadUrlResult {
  url: string
  fileName: string
  expiresInSeconds: number
}

export function getBackupSettings() {
  return request<BackupSettingsView>({
    url: '/api/admin/settings/backups',
    method: 'GET',
  })
}

export function updateBackupStorage(data: UpdateBackupStoragePayload) {
  return request<BackupSettingsView>({
    url: '/api/admin/settings/backups/storage/update',
    method: 'POST',
    data,
  })
}

export function testBackupStorage() {
  return request<ConnectionTestResult>({
    url: '/api/admin/settings/backups/storage/test',
    method: 'POST',
  })
}

export function updateBackupSchedule(data: UpdateBackupSchedulePayload) {
  return request<BackupSettingsView>({
    url: '/api/admin/settings/backups/schedule/update',
    method: 'POST',
    data,
  })
}

export function getBackupRecords(params: BackupRecordsParams) {
  return request<BackupRecordPage>({
    url: '/api/admin/settings/backups/records',
    method: 'GET',
    params,
  })
}

export function createBackup() {
  return request<BackupRecord>({
    url: '/api/admin/settings/backups/create',
    method: 'POST',
  })
}

export function getBackupDownloadUrl(backupId: string) {
  return request<DownloadUrlResult>({
    url: '/api/admin/settings/backups/download-url',
    method: 'POST',
    data: { backupId },
  })
}

export function deleteBackup(backupId: string) {
  return request<BackupRecord>({
    url: '/api/admin/settings/backups/delete',
    method: 'POST',
    data: { backupId },
  })
}
