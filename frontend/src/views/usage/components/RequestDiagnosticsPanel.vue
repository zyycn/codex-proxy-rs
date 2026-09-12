<script setup lang="ts">
import type { OpsError, OpsErrorMetadata } from '@/api'
import { Download, RefreshCw } from '@lucide/vue'
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useRequestDiagnostics } from '../composables/useRequestDiagnostics'
import { requestDiagnosticsBundle } from '../utils/diagnosticsBundle'
import RequestTransportFailure from './RequestTransportFailure.vue'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'

const props = defineProps<{ requestId: string, metadata?: OpsErrorMetadata, errorRecord?: OpsError }>()
const { downloadJson } = useDownload()
const { loading: exporting, run: runExport } = useAsyncAction()
const { selectedId, detail, loading, error, refresh } = useRequestDiagnostics(() => props.requestId)
const trace = computed(() => detail.value?.trace)
const events = computed(() => (trace.value?.events ?? []).map(event => ({
  ...event,
  content: JSON.stringify(event.data, null, 2),
})))
const selectedError = computed(() => props.errorRecord?.requestId === selectedId.value ? props.errorRecord : undefined)
const canExport = computed(() => !loading.value && (detail.value?.requestId === selectedId.value || !!selectedError.value))

function download() {
  if (!canExport.value || exporting.value)
    return
  const bundle = requestDiagnosticsBundle(selectedId.value, detail.value, selectedError.value)
  return runExport(
    () => downloadJson(bundle, `diagnostics-${selectedId.value.replace(/[^\w-]/g, '_')}.json`),
    { errorText: '导出诊断包失败', minimumMs: 400 },
  )
}
</script>

<template>
  <section class="mt-3 min-w-0 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5" aria-label="请求诊断">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <h3 class="m-0 text-cp-sm font-heavy text-cp-text-secondary">
        请求诊断
      </h3>
      <div class="flex flex-wrap gap-2">
        <BaseButton v-if="selectedId !== requestId" variant="soft" size="sm" @click="selectedId = requestId">
          返回本次请求
        </BaseButton>
        <BaseButton variant="soft" size="sm" :loading="loading" @click="refresh">
          <template #icon>
            <RefreshCw :size="14" />
          </template>
          刷新
        </BaseButton>
        <BaseButton variant="soft" size="sm" :loading="exporting" :disabled="!canExport" @click="download">
          <template #icon>
            <Download :size="14" />
          </template>
          导出诊断包
        </BaseButton>
      </div>
    </div>
    <p class="mt-1 mb-3 break-all font-mono text-cp-xs leading-relaxed text-cp-text-secondary">
      {{ selectedId }}
    </p>
    <p v-if="loading" role="status" class="text-cp-sm text-cp-text-secondary">
      正在加载诊断记录…
    </p>
    <p v-else-if="error" role="alert" class="text-cp-sm text-cp-error-text">
      {{ error }}
    </p>
    <template v-else-if="detail">
      <div v-if="detail.relatedRequests?.length" class="mb-3 flex flex-wrap gap-2">
        <BaseButton v-for="related in detail.relatedRequests" :key="related.requestId" variant="soft" size="sm" class="max-w-full" @click="selectedId = related.requestId">
          {{ related.relation === 'recovered_by' ? '查看恢复请求' : '查看先前失败' }} · {{ related.requestId }}
        </BaseButton>
      </div>
      <RequestTransportFailure :events="trace?.events ?? []" :metadata="selectedId === requestId ? metadata : undefined" />
      <BaseEmpty v-if="!events.length" title="暂无诊断时间线" size="sm" surface="none" />
      <template v-else-if="trace">
        <div class="mb-3 grid gap-1 text-cp-xs leading-relaxed">
          <p class="m-0 text-cp-text-secondary">
            已观测 {{ trace.totalEvents }} 个阶段或事件，展示 {{ events.length }} 条记录
          </p>
          <p v-if="trace.droppedEvents" role="status" class="m-0 text-cp-warning-text">
            已达保存上限，{{ trace.droppedEvents }} 个事件未保留
          </p>
        </div>
        <BaseScrollbar max-height="32rem">
          <ol class="m-0 grid list-none gap-2 p-0 pr-3">
            <li v-for="event in events" :key="event.sequence" class="min-w-0">
              <details class="group overflow-hidden rounded-cp bg-cp-bg-container">
                <summary class="cursor-pointer px-3 py-2.5 break-all text-cp-xs leading-relaxed transition-colors hover:bg-cp-primary-container group-open:bg-cp-primary-container focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-cp-primary motion-reduce:transition-none">
                  <span class="font-mono tabular-nums text-cp-text-secondary">+{{ event.elapsedMs }} ms · #{{ event.sequence }}</span>
                  <span class="mx-2 font-mono text-cp-text">{{ event.stage }}</span>
                  <span v-if="event.attemptIndex" class="text-cp-text-secondary">尝试 {{ event.attemptIndex }}</span>
                  <span v-if="event.exchangeId" class="text-cp-text-secondary"> · 交换 {{ event.exchangeId }}</span>
                  <span v-if="event.count > 1" class="text-cp-text-secondary"> · {{ event.count }} 次（至 +{{ event.lastElapsedMs }} ms）</span>
                </summary>
                <div class="mx-3 py-3">
                  <UsageDetailCodePanel title="诊断事实" :content="event.content" max-height="280px" />
                </div>
              </details>
            </li>
          </ol>
        </BaseScrollbar>
      </template>
    </template>
  </section>
</template>
