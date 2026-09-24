import type { RequestOptions } from '../request'

import { API_BASE_URL } from '../constants'
import request from '../request'

export interface PluginSourceEgress {
  id: string
  revision: number
}

export type PluginSource
  = | { kind: 'builtin', release: string }
    | { kind: 'upload' }
    | { kind: 'url', url: string, credential_ids: string[], outbound_proxy?: PluginSourceEgress }
    | { kind: 'github', repository: string, tag: string, asset: string, credential_ids: string[], outbound_proxy?: PluginSourceEgress }

export interface PluginContribution {
  id: string
  version: number
  stages: string[]
  inputFormats: string[]
  outputFormats: string[]
}

export type PluginIconAsset = string | { light: string, dark: string }

export interface PluginPermissionDescription {
  permission: string
  label: string
  description: string
}

export interface PluginArtifactMetadata {
  pluginId: string
  version: string
  name: string
  displayName: string
  publisher: string
  author: string | null
  description: string
  license: string
  icon: PluginIconAsset | null
  sha256: string
  platforms: string[]
  contributes: Record<string, PluginContribution>
  requestedPermissions: string[]
  permissionDescriptions: PluginPermissionDescription[]
  configurationSchema: Record<string, unknown>
  secretFields: string[]
  stateNamespaces: {
    namespace: string
    schemaVersion: number
    schemaSha256: string
    schema: Record<string, unknown>
    maximumRecords: number
    maximumBytes: number
    maximumValueBytes: number
    migratesFrom: number[]
  }[]
}

export interface PluginArtifact {
  metadata: PluginArtifactMetadata
  source: PluginSource
  installedAt: string
  acceptedAt: string | null
}

export type PluginUpdateSource
  = | { kind: 'builtin' }
    | { kind: 'upload' }
    | { kind: 'url', url: string }
    | { kind: 'github', repository: string }

export interface PluginUpdateSourceBinding {
  pluginId: string
  source: PluginUpdateSource
  policy: PluginUpdatePolicy
  outboundProxyId?: string | null
}

export type PluginUpdatePolicy
  = | { kind: 'manual' }
    | { kind: 'stable' }
    | { kind: 'pinned', tag: string, allow_prerelease: boolean }

export interface PluginUpdateCheck {
  binding: PluginUpdateSourceBinding
  release: PluginRelease
}

export type DownloadPurpose = 'metadata' | 'artifact'

export interface PluginSourceCredential {
  id: string
  name: string
  origin: string
  pathPrefix: string
  purposes: DownloadPurpose[]
}

export type PluginSourceAuthentication
  = | { kind: 'github', token: string }
    | { kind: 'bearer', token: string }
    | { kind: 'basic', username: string, password: string }
    | { kind: 'header', name: string, value: string }

export interface CreatePluginSourceCredentialRequest {
  name: string
  origin: string
  pathPrefix: string
  purposes: DownloadPurpose[]
  authentication: PluginSourceAuthentication
}

export interface GithubReleaseQuery {
  repository: string
  tag: string | null
  allowPrerelease: boolean
}

export interface PluginReleaseAsset {
  name: string
  size: number
  sha256: string | null
}

export interface PluginRelease {
  repository: string
  tag: string
  name: string
  prerelease: boolean
  assets: PluginReleaseAsset[]
  queriedAt: string
  expiresAt: string
}

export interface QueryPluginReleaseRequest {
  query: GithubReleaseQuery
  credentialIds: string[]
  outboundProxyId?: string | null
}

export type RemotePluginLocation
  = | { kind: 'url', url: string, sha256: string | null }
    | {
      kind: 'github'
      repository: string
      tag: string
      asset: string
      allow_prerelease: boolean
      sha256: string | null
    }

interface RemotePluginRequest {
  credentialIds: string[]
  outboundProxyId?: string | null
  location: RemotePluginLocation
}

export interface InstallRemotePluginRequest extends RemotePluginRequest {
  pluginId: string
  version: string
  location: RemotePluginLocation & { sha256: string }
}

export interface VerifyRemotePluginRequest extends RemotePluginRequest {
  expectedPluginId?: string | null
}

export interface VerifiedPluginArtifact {
  metadata: PluginArtifactMetadata
  source: PluginSource
}

export type PluginFailurePolicy = 'reject' | 'delegate' | 'observe'

export interface PluginFrontendIdentityBinding {
  principal: string
  clientKeyId: string
}

export interface PluginCapabilityBinding {
  contribution: string
  stage: string
  order: number
  failurePolicy: PluginFailurePolicy
  providerIds: string[]
  models: string[]
  clientKeyIds: string[]
  accountGroupIds: string[]
  identityBindings: PluginFrontendIdentityBinding[]
}

export type PluginInstanceRuntimeStatus
  = | 'disabled'
    | 'awaiting_publication'
    | 'preparing'
    | 'running'
    | 'blocked'
    | 'preparation_failed'
    | 'faulted'
    | 'draining'

export interface PluginInstanceRuntimeFailure {
  code: string
  message: string
}

export interface PluginInstanceRuntime {
  status: PluginInstanceRuntimeStatus
  actualRevision: number | null
  actualArtifactSha256: string | null
  failure: PluginInstanceRuntimeFailure | null
  drainingRevisions: number[]
  retainedContinuations: number
}

export interface PluginInstance {
  id: string
  name: string
  artifactSha256: string
  enabled: boolean
  configurationRequired: boolean
  configuration: Record<string, unknown>
  secretFields: string[]
  bindings: PluginCapabilityBinding[]
  revision: number
  running: boolean
  publishedRevision: number | null
  runtime: PluginInstanceRuntime
}

export interface ConfigurePluginInstanceRequest {
  creationId?: string
  expectedRevision?: number
  replaceInstances?: { id: string, expectedRevision: number }[]
  name: string
  artifactSha256: string
  enabled: boolean
  configuration: Record<string, unknown>
  secrets?: Record<string, string>
  bindings: PluginCapabilityBinding[]
}

export interface PluginMutationResponse {
  configRevision: number
}

export interface PluginArtifactMutationResponse extends PluginMutationResponse {
  artifact: PluginArtifact
  defaultInstanceId: string | null
  configurationRequired: boolean
}

interface PluginInstanceMutationResponse extends PluginMutationResponse {
  id: string
}

export interface PluginRollbackPlan {
  instanceRevision: number
  currentVersion: string
  targets: { artifactSha256: string, version: string, platforms: string[] }[]
}

export function getPluginArtifacts(options: RequestOptions = {}) {
  return request<PluginArtifact[]>({
    url: '/api/admin/plugins/artifacts',
    method: 'GET',
    ...options,
  })
}

export function pluginArtifactIconPath(sha256: string, theme: 'light' | 'dark') {
  return `${API_BASE_URL}/api/admin/plugins/artifacts/${encodeURIComponent(sha256)}/icon?theme=${theme}`
}

export function uploadPluginArtifact(file: File, sha256: string, options: RequestOptions = {}) {
  return request<PluginArtifactMutationResponse>({
    url: '/api/admin/plugins/artifacts/upload',
    method: 'POST',
    params: { sha256 },
    data: file,
    headers: {
      'Content-Type': 'application/octet-stream',
    },
    timeout: 120000,
    ...options,
  })
}

export function verifyUploadedPlugin(file: File, options: RequestOptions = {}) {
  return request<VerifiedPluginArtifact>({
    url: '/api/admin/plugins/artifacts/upload/verify',
    method: 'POST',
    data: file,
    headers: {
      'Content-Type': 'application/octet-stream',
    },
    timeout: 120000,
    ...options,
  })
}

export function installRemotePlugin(data: InstallRemotePluginRequest, options: RequestOptions = {}) {
  return request<PluginArtifactMutationResponse>({
    url: '/api/admin/plugins/artifacts/install',
    method: 'POST',
    data,
    timeout: 120000,
    ...options,
  })
}

export function acceptPluginArtifact(data: { sha256: string }, options: RequestOptions = {}) {
  return request<PluginArtifactMutationResponse>({
    url: '/api/admin/plugins/artifacts/accept',
    method: 'POST',
    data,
    ...options,
  })
}

export function verifyRemotePlugin(data: VerifyRemotePluginRequest, options: RequestOptions = {}) {
  return request<VerifiedPluginArtifact>({
    url: '/api/admin/plugins/artifacts/verify',
    method: 'POST',
    data,
    timeout: 120000,
    ...options,
  })
}

export function deletePluginArtifact(data: { sha256: string }, options: RequestOptions = {}) {
  return request<void>({
    url: '/api/admin/plugins/artifacts/delete',
    method: 'POST',
    data,
    ...options,
  })
}

export function queryPluginRelease(data: QueryPluginReleaseRequest, options: RequestOptions = {}) {
  return request<PluginRelease>({
    url: '/api/admin/plugins/releases/query',
    method: 'POST',
    data,
    timeout: 30000,
    ...options,
  })
}

export function getPluginSourceCredentials(options: RequestOptions = {}) {
  return request<PluginSourceCredential[]>({
    url: '/api/admin/plugins/source-credentials',
    method: 'GET',
    ...options,
  })
}

export function createPluginSourceCredential(data: CreatePluginSourceCredentialRequest, options: RequestOptions = {}) {
  return request<PluginSourceCredential>({
    url: '/api/admin/plugins/source-credentials',
    method: 'POST',
    data,
    ...options,
  })
}

export function deletePluginSourceCredential(data: { id: string }, options: RequestOptions = {}) {
  return request<PluginMutationResponse>({
    url: '/api/admin/plugins/source-credentials/delete',
    method: 'POST',
    data,
    ...options,
  })
}

export function getPluginUpdateSources(options: RequestOptions = {}) {
  return request<PluginUpdateSourceBinding[]>({
    url: '/api/admin/plugins/update-sources',
    method: 'GET',
    ...options,
  })
}

export function updatePluginSource(data: PluginUpdateSourceBinding, options: RequestOptions = {}) {
  return request<PluginMutationResponse>({
    url: '/api/admin/plugins/update-sources',
    method: 'POST',
    data,
    ...options,
  })
}

export function checkPluginUpdate(data: { pluginId: string, credentialIds: string[] }, options: RequestOptions = {}) {
  return request<PluginUpdateCheck>({
    url: '/api/admin/plugins/updates/check',
    method: 'POST',
    data,
    ...options,
  })
}

export function getPluginInstances(options: RequestOptions = {}) {
  return request<PluginInstance[]>({
    url: '/api/admin/plugins/instances',
    method: 'GET',
    ...options,
  })
}

export function createPluginInstance(data: ConfigurePluginInstanceRequest, options: RequestOptions = {}) {
  return request<PluginInstanceMutationResponse>({
    url: '/api/admin/plugins/instances',
    method: 'POST',
    data,
    ...options,
  })
}

export function updatePluginInstance(data: { id: string, instance: ConfigurePluginInstanceRequest }, options: RequestOptions = {}) {
  return request<PluginInstanceMutationResponse>({
    url: '/api/admin/plugins/instances/update',
    method: 'POST',
    data,
    ...options,
  })
}

export function getPluginRollbackPlan(id: string, options: RequestOptions = {}) {
  return request<PluginRollbackPlan>({
    url: '/api/admin/plugins/instances/rollback-plan',
    method: 'GET',
    params: { id },
    ...options,
  })
}

export function rollbackPluginInstance(data: { id: string, target: { artifactSha256: string, expectedRevision: number } }, options: RequestOptions = {}) {
  return request<PluginInstanceMutationResponse>({
    url: '/api/admin/plugins/instances/rollback',
    method: 'POST',
    data,
    ...options,
  })
}

export function disablePluginInstance(data: { id: string }, options: RequestOptions = {}) {
  return request<PluginMutationResponse>({
    url: '/api/admin/plugins/instances/disable',
    method: 'POST',
    data,
    ...options,
  })
}

export function deletePluginInstance(data: { id: string }, options: RequestOptions = {}) {
  return request<PluginMutationResponse>({
    url: '/api/admin/plugins/instances/delete',
    method: 'POST',
    data,
    ...options,
  })
}

export interface PluginVersionPlan {
  instanceRevision: number
  artifactSha256: string
  configuration: Record<string, unknown>
  secretFields: string[]
  bindings: PluginCapabilityBinding[]
  restored: boolean
}

export function getPluginVersionPlan(id: string, artifactSha256: string, options: RequestOptions = {}) {
  return request<PluginVersionPlan>({
    url: '/api/admin/plugins/instances/version-plan',
    method: 'GET',
    params: { id, artifactSha256 },
    ...options,
  })
}

export function switchPluginVersion(data: { id: string, target: { artifactSha256: string, expectedRevision: number } }, options: RequestOptions = {}) {
  return request<PluginInstanceMutationResponse>({
    url: '/api/admin/plugins/instances/switch-version',
    method: 'POST',
    data,
    timeout: 120000,
    ...options,
  })
}
