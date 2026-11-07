<script setup lang="ts">
import type { ProxyBatchInput } from '../utils/batch'

import { ZForm, ZFormItem, ZIconButton, ZInput, ZPopover, ZSelect, ZTextarea } from '@codex-proxy/ui'
import { CircleHelp } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { MAX_BATCH_CREATE, proxyProtocolOptions } from '../utils/batch'

const props = defineProps<{ rows: ProxyBatchInput[], error: string, disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const text = defineModel<string>({ required: true })
const prefix = defineModel<string>('prefix', { required: true })
const protocol = defineModel<string>('protocol', { required: true })
const invalid = computed(() => props.rows.filter(row => row.error))
</script>

<template>
  <ZForm class="grid gap-5">
    <ZFormItem :label="t('proxies.batchCreate.proxyList')" required :error="error || undefined">
      <template #label-extra>
        <ZPopover class="-my-1" trigger="hover-click" placement="bottom-start">
          <template #reference="{ open }">
            <ZIconButton variant="link" :aria-label="t('proxies.batchCreate.formatDescription')" :aria-expanded="open" class="size-6!">
              <CircleHelp class="size-3.5" aria-hidden="true" />
            </ZIconButton>
          </template>
          <div class="max-w-80 space-y-3 p-3 text-z-sm text-z-text-secondary">
            <p class="m-0">
              {{ t('proxies.batchCreate.supportedFormats') }}
            </p>
            <div class="space-y-1 font-mono text-z-xs break-all text-z-text">
              <p class="m-0">
                host:port
              </p>
              <p class="m-0">
                {{ t('proxies.batchCreate.hostPortUsernamePassword') }}
              </p>
              <p class="m-0">
                {{ t('proxies.batchCreate.httpUsernamePasswordHostPort') }}
              </p>
            </div>
            <p class="m-0">
              {{ t('proxies.batchCreate.protocolHelp') }}
            </p>
            <p class="m-0">
              {{ t('proxies.batchCreate.ipv6Help') }} <code>[::1]:8080</code>
            </p>
          </div>
        </ZPopover>
      </template>
      <template #extra>
        <span class="text-z-xs text-z-text-tertiary">{{ t('proxies.batchCreate.onePerLineMaximum', { value: MAX_BATCH_CREATE }) }}</span>
      </template>
      <ZTextarea v-model="text" :rows="8" :disabled="disabled" class="font-mono" :aria-label="t('proxies.batchCreate.bulkProxyList')" autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="proxy.example.com:8080&#10;proxy.example.net:1080:user:password&#10;socks5://user:password@proxy.example.org:1080" />
      <div v-if="rows.length && !error" class="mt-2 space-y-1.5 text-z-xs">
        <p class="m-0 text-z-text-secondary" role="status">
          {{ t('proxies.batchCreate.totalReadyToAdd', { count: rows.length, count2: rows.length - invalid.length }) }}<span v-if="invalid.length" class="text-z-error-text">{{ t('proxies.batchCreate.needsChanges', { count: invalid.length }) }}</span>
        </p>
        <ul v-if="invalid.length" class="m-0 list-none space-y-1 p-0 text-z-error-text" role="alert">
          <li v-for="row in invalid.slice(0, 5)" :key="row.line">
            {{ t('proxies.batchCreate.line', { line: row.line, error: row.error }) }}
          </li>
          <li v-if="invalid.length > 5">
            {{ t('proxies.batchCreate.additionalIssues', { count: invalid.length - 5 }) }}
          </li>
        </ul>
      </div>
    </ZFormItem>
    <div class="grid items-start gap-4 sm:grid-cols-[10rem_minmax(0,1fr)]">
      <ZFormItem :label="t('proxies.batchCreate.defaultProtocol')">
        <ZSelect v-model="protocol" :options="proxyProtocolOptions" :disabled="disabled" :aria-label="t('proxies.batchCreate.defaultProtocolLabel')" class="w-full" />
      </ZFormItem>
      <ZFormItem :label="t('proxies.batchCreate.namePrefix')">
        <ZInput v-model="prefix" maxlength="96" :disabled="disabled" :aria-label="t('proxies.batchCreate.namePrefix')" :placeholder="t('proxies.batchCreate.proxy')" />
      </ZFormItem>
    </div>
  </ZForm>
</template>
