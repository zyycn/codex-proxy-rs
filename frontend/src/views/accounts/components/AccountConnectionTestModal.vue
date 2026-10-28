<script setup lang="ts">
import type { useAccountConnectionTest } from '../composables/useAccountConnectionTest'

import { ZButton, ZDialog, ZIconButton, ZSelect } from '@codex-proxy/ui'

import { RefreshCw } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import AccountStatusBadge from './account-status-badge/index.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'

defineProps<{
  account: ConnectionTest['testingAccount']['value']
  status: ConnectionTest['connectionTestStatus']['value']
  model: string
  logs: ConnectionTest['connectionTestLogs']['value']
  error: string
  startedAt: string
  finishedAt: string
  durationMs: number | null
  loadingModels: boolean
  refreshingModels: boolean
  modelOptions: ConnectionTest['connectionTestModelOptions']['value']
  statusView: ConnectionTest['connectionTestStatusView']['value']
}>()

const emit = defineEmits<{
  test: []
  refreshModels: []
}>()

const { t } = useI18n({ useScope: 'global' })

type ConnectionTest = ReturnType<typeof useAccountConnectionTest>

const open = defineModel<boolean>({ default: false })
const selectedModel = defineModel<string>('selectedModel', { required: true })

function connectionLogClass(tone: string) {
  if (tone === 'success')
    return 'text-cp-success-text'
  if (tone === 'danger')
    return 'text-cp-error-text'
  if (tone === 'info')
    return 'text-cp-info-text'
  return 'text-cp-text-secondary'
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('accounts.connectionTest.title')"
    :description="t('accounts.connectionTest.description')"
    type="info"
    width="48rem"
  >
    <div v-if="account" class="flex flex-col gap-4">
      <section
        class="flex items-center justify-between gap-4 rounded-cp-card bg-cp-fill-quaternary px-4 py-3"
      >
        <AccountIdentityCell :account="account" size="lg" show-plan />
        <AccountStatusBadge
          :status="account.status"
          :error-reason="account.errorReason"
          :error-message="account.errorMessage"
          :rate-limit-recovery-display="account.quota.rateLimitRecoveryDisplay"
          :rate-limit-reason="account.quota.rateLimitReason"
          :protection-rules="account.quota.protectionRules"
          :recovery-probe-required="account.quota.recoveryProbeRequired"
          :next-refresh-at="account.nextRefreshAt"
          :next-refresh-at-display="account.nextRefreshAtDisplay"
          variant="pill"
        />
      </section>

      <section class="rounded-cp-card bg-cp-fill-quaternary px-4 py-3">
        <div class="grid gap-2">
          <div class="flex min-h-8 items-center justify-between gap-3">
            <span class="text-cp-sm font-heavy text-cp-text-quaternary">
              {{ t('accounts.connectionTest.model') }}
            </span>
            <ZIconButton
              size="small"
              :aria-label="t('accounts.connectionTest.refreshModels')"
              :loading="refreshingModels"
              :disabled="status === 'running' || loadingModels"
              @click="emit('refreshModels')"
            >
              <template #loading>
                <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
              </template>
              <RefreshCw class="size-3.5" />
            </ZIconButton>
          </div>
          <ZSelect
            v-model="selectedModel"
            :aria-label="t('accounts.connectionTest.model')"
            :options="modelOptions"
            :disabled="status === 'running' || loadingModels || refreshingModels"
            :placeholder="loadingModels ? t('accounts.connectionTest.loadingModels') : t('accounts.connectionTest.modelPlaceholder')"
            :empty-text="t('accounts.connectionTest.noModels')"
          />
        </div>
      </section>

      <section class="rounded-cp-card bg-cp-fill-quaternary p-4">
        <div class="flex min-w-0 items-start gap-3">
          <span
            class="inline-flex size-10 shrink-0 items-center justify-center rounded-lg"
            :class="statusView.badge"
          >
            <component
              :is="statusView.icon"
              class="size-5"
              :class="[statusView.iconClass, status === 'running' ? 'animate-pulse' : '']"
            />
          </span>
          <div class="min-w-0">
            <p class="m-0 text-[16px] font-heavy text-cp-text">
              {{ statusView.label }}
            </p>
            <p
              class="mt-1.5 mb-0 text-cp leading-normal font-emphasis text-cp-text-secondary"
            >
              {{ statusView.description }}
            </p>
          </div>
        </div>

        <div class="mt-4 grid gap-3 sm:grid-cols-3">
          <div class="rounded-lg bg-cp-bg-container px-3 py-2.5">
            <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
              {{ t('accounts.concurrency.startTime') }}
            </p>
            <p class="mt-1.5 mb-0 font-mono text-cp-sm font-emphasis text-cp-text">
              {{ startedAt || '-' }}
            </p>
          </div>
          <div class="rounded-lg bg-cp-bg-container px-3 py-2.5">
            <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
              {{ t('accounts.connectionTest.completedAt') }}
            </p>
            <p class="mt-1.5 mb-0 font-mono text-cp-sm font-emphasis text-cp-text">
              {{ finishedAt || '-' }}
            </p>
          </div>
          <div class="rounded-lg bg-cp-bg-container px-3 py-2.5">
            <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
              {{ t('accounts.connectionTest.duration') }}
            </p>
            <p class="mt-1.5 mb-0 font-mono text-cp-sm font-emphasis whitespace-nowrap text-cp-text">
              {{ durationMs !== null ? `${durationMs} ms` : '-' }}
            </p>
          </div>
        </div>

        <div class="mt-3 rounded-lg bg-cp-bg-container px-3 py-2.5">
          <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
            {{ t('accounts.connectionTest.model') }}
          </p>
          <p
            class="mt-1.5 mb-0 truncate font-mono text-cp-sm font-emphasis text-cp-text"
            :title="model || '-'"
          >
            {{ model || '-' }}
          </p>
        </div>

        <div class="mt-3 rounded-lg bg-cp-bg-container px-3 py-2.5">
          <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
            {{ t('accounts.connectionTest.events.title') }}
          </p>
          <div class="pt-2">
            <div v-if="logs.length === 0" class="text-cp-sm font-emphasis text-cp-text-quaternary">
              -
            </div>
            <div v-else class="flex flex-col gap-1.5">
              <div
                v-for="item in logs"
                :key="item.key"
                class="grid grid-cols-[54px_minmax(0,1fr)] gap-2 text-cp-sm leading-[1.45] font-emphasis"
              >
                <span class="font-mono text-cp-text-quaternary">{{ item.time }}</span>
                <div class="min-w-0">
                  <p
                    class="m-0 wrap-break-word"
                    :class="connectionLogClass(item.tone)"
                  >
                    {{ item.text }}
                  </p>
                  <div v-if="item.detail" class="mt-2 rounded-lg bg-cp-fill-quaternary px-3 py-2">
                    <p
                      v-if="item.tone === 'danger'"
                      class="mt-0 mb-2 text-cp-xs font-heavy text-cp-text-quaternary"
                    >
                      {{ t('accounts.connectionTest.diagnostics') }}
                    </p>
                    <pre
                      class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-xs leading-[1.6] font-emphasis text-cp-text"
                      v-text="item.detail"
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>
    </div>

    <template #footer>
      <ZButton @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
      <ZButton
        type="primary"
        :loading="status === 'running'"
        :disabled="!account || loadingModels || refreshingModels || !selectedModel"
        @click="emit('test')"
      >
        {{ logs.length > 0 || error ? t('accounts.connectionTest.retry') : t('accounts.connectionTest.start') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
