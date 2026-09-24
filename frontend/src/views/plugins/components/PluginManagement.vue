<script setup lang="ts">
import { ArrowInDownSquareHalf } from '@boxicons/vue'
import { BaseCheckbox, BaseConfirmModal, BaseIconButton, BasePageHeader, BaseScrollbar, BaseSegmented } from '@codex-proxy/ui'

import { PackageOpen, PanelsTopLeft, RefreshCw } from '@lucide/vue'
import { shallowRef } from 'vue'
import { usePluginManagement } from '../composables/usePluginManagement'
import PluginCatalogPanel from './PluginCatalogPanel.vue'
import PluginConfigurationModal from './PluginConfigurationModal.vue'
import PluginDetailModal from './PluginDetailModal.vue'
import PluginEnableModal from './PluginEnableModal.vue'
import PluginExtensionsPanel from './PluginExtensionsPanel.vue'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginInstallModal from './PluginInstallModal.vue'
import PluginRollbackModal from './PluginRollbackModal.vue'
import PluginUpdateCheckModal from './PluginUpdateCheckModal.vue'

const activeTab = shallowRef<'artifacts' | 'extensions'>('artifacts')
const management = usePluginManagement()

const tabOptions = [
  { label: '已安装', value: 'artifacts', icon: PackageOpen },
  { label: '扩展页', value: 'extensions', icon: PanelsTopLeft },
]
</script>

<template>
  <div class="flex h-[calc(100dvh-2rem)] min-h-0 w-full flex-none! flex-col gap-3 overflow-hidden min-[961px]:h-[calc(100dvh-3rem)]">
    <BasePageHeader title="插件管理" description="管理插件、版本和扩展页面" />

    <div class="flex shrink-0 items-start gap-3">
      <BaseScrollbar horizontal :vertical="false" height="var(--cp-control-height)" class="min-w-0">
        <BaseSegmented
          v-model="activeTab"
          :options="tabOptions"
          label="插件管理分区"
          class="w-44"
        />
      </BaseScrollbar>
      <div class="flex shrink-0 items-center gap-1">
        <BaseIconButton label="安装插件" variant="secondary" @click="management.openInstall('upload')">
          <ArrowInDownSquareHalf pack="filled" class="size-5" />
        </BaseIconButton>
        <BaseIconButton label="刷新插件" variant="secondary" :loading="management.loading.value" @click="management.refresh()">
          <template #loading>
            <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4.5" />
        </BaseIconButton>
      </div>
    </div>

    <div class="flex min-h-0 flex-1 flex-col gap-3">
      <PluginCatalogPanel
        v-if="activeTab === 'artifacts'"
        class="min-h-0 flex-1"
        :plugins="management.catalog.value"
        :loading="management.loading.value"
        @manage="management.openDetail($event.id)"
      />
      <PluginExtensionsPanel
        v-else
        class="min-h-0 flex-1"
        :views="management.extensions.value"
        :loading="management.loading.value"
      />
    </div>

    <PluginDetailModal
      v-model="management.showDetail.value"
      :plugin="management.selectedPlugin.value"
      :initial-section="management.detailSection.value"
      :views="management.extensions.value"
      :busy="management.savingInstance.value || management.uninstall.busy.value || Boolean(management.busyInstanceId.value || management.busyDigest.value)"
      @accept="management.openAcceptance"
      @edit="management.openEditInstance"
      @enable="management.requestInstanceEnable"
      @disable="management.requestInstanceDisable"
      @delete-configuration="management.requestInstanceDelete"
      @delete-version="management.requestArtifactDelete"
      @rollback="management.openRollback"
      @install-version="management.openVersionInstall"
      @check-update="management.updateCheck.check"
      @switch-version="management.requestVersionSwitch"
      @uninstall="management.uninstall.request"
    />

    <PluginInstallModal
      v-model="management.showInstall.value"
      :mode="management.installMode.value"
      :installed-artifacts="management.artifacts.value"
      :acceptance-artifact="management.acceptanceArtifact.value"
      :credentials="management.credentials.value"
      :saving-credential="management.savingCredential.value"
      :save-credential="management.saveCredential"
      :release="management.release.value"
      :update-source="management.updateSource.value"
      :update-selection="management.updateSelection.value"
      :save-source="management.saveInstallSource"
      :saving="management.installing.value"
      :querying="management.queryingRelease.value"
      :verifying="management.verifyingArtifact.value"
      :verified="management.verifiedArtifact.value"
      @install="management.installArtifact"
      @accept="management.acceptArtifact"
      @query="management.queryRelease"
      @verify="management.verifyArtifact"
      @reset-verification="management.resetArtifactVerification"
      @reset-query="management.resetReleaseQuery"
      @change-mode="management.changeInstallMode"
      @delete-credential="management.requestCredentialDelete"
    />

    <PluginUpdateCheckModal
      v-model="management.updateCheck.open.value"
      :plugin="management.updateCheck.plugin.value"
      :result="management.updateCheck.result.value"
      :checking="management.updateCheck.checking.value"
      @install="management.updateCheck.open.value = false; management.openCheckedUpdate($event)"
    />

    <PluginConfigurationModal
      v-model="management.showInstance.value"
      :instance="management.editingInstance.value"
      :artifact="management.configurationArtifact.value"
      :draft="management.configurationDraft.value"
      :error="management.configurationError.value"
      :saving="management.savingInstance.value"
      @save="management.saveInstance"
    />

    <BaseConfirmModal
      v-model="management.showVersionSwitch.value"
      title="切换插件版本"
      description="切换前会检查目标版本设置，失败时保留当前版本"
      confirm-text="确认切换"
      :loading="Boolean(management.busyInstanceId.value)"
      @confirm="management.confirmVersionSwitch"
    >
      <p class="m-0 text-cp-sm">
        {{ management.pendingVersionSwitch.value?.artifact.metadata.displayName }}：
        {{ management.pendingVersionSwitch.value?.currentVersion }} → {{ management.pendingVersionSwitch.value?.artifact.metadata.version }}
      </p>
    </BaseConfirmModal>

    <PluginRollbackModal
      v-model="management.showRollback.value"
      :instance="management.rollbackInstance.value"
      :plan="management.rollbackPlan.value"
      :loading="management.loadingRollback.value"
      :saving="Boolean(management.busyInstanceId.value)"
      @confirm="management.confirmRollback"
      @reload="management.openRollback"
    />

    <PluginEnableModal
      v-model="management.showEnableConfirmation.value"
      :request="management.pendingEnable.value?.request"
      :metadata="management.pendingEnable.value?.artifact.metadata"
      :replacements="management.pendingEnable.value?.replacements ?? []"
      :saving="management.savingInstance.value"
      @confirm="management.confirmInstanceEnable"
    />

    <BaseConfirmModal
      v-model="management.showArtifactDelete.value"
      title="删除插件版本"
      description="使用中的版本不可删除，删除版本也会清除对应的恢复设置"
      destructive
      confirm-text="确认删除"
      :loading="Boolean(management.busyDigest.value)"
      @confirm="management.confirmArtifactDelete"
    >
      <div class="flex min-w-0 items-center gap-2 text-cp-sm font-normal">
        <p class="m-0 min-w-0 wrap-anywhere">
          删除 {{ management.pendingArtifact.value?.metadata.displayName }} {{ management.pendingArtifact.value?.metadata.version }}？
        </p>
        <PluginHelpPopover label="版本删除详情">
          <p class="m-0">
            被配置或运行中的调用引用时不能删除，不会自动停用其他配置
          </p>
          <p class="m-0">
            只清理此版本独用的下载认证，删除最后一个版本时同时清理来源规则，共享认证与代理不受影响
          </p>
          <p class="m-0">
            SHA-256
          </p>
          <p class="m-0 break-all font-mono">
            {{ management.pendingArtifact.value?.metadata.sha256 }}
          </p>
        </PluginHelpPopover>
      </div>
      <p class="mt-3 mb-0 text-cp-xs font-normal text-cp-text-secondary">
        同时清理独用下载认证，最后一版还会清理来源
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="management.showInstanceDelete.value"
      title="删除插件配置"
      description="配置、密钥与私有数据将永久删除"
      destructive
      confirm-text="确认删除"
      :loading="Boolean(management.busyInstanceId.value)"
      @confirm="management.confirmInstanceDelete"
    >
      <div class="grid gap-3 text-cp-sm font-normal">
        <p class="m-0 wrap-anywhere">
          {{ management.pendingDeleteInstance.value?.name }}
        </p>
        <p class="m-0 text-cp-xs">
          无法恢复，已安装版本与其他配置不受影响
        </p>
      </div>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="management.uninstall.open.value"
      title="卸载插件"
      description="删除全部版本、配置、密钥与私有数据"
      destructive
      confirm-text="确认卸载"
      :loading="management.uninstall.busy.value"
      :confirm-disabled="!management.uninstall.acknowledged.value"
      @confirm="management.uninstall.confirm"
    >
      <div class="grid gap-3 text-cp-sm font-normal">
        <p class="m-0 wrap-anywhere">
          {{ management.uninstall.pending.value?.artifact.metadata.displayName }} · {{ management.uninstall.pending.value?.artifacts.length }} 个版本
        </p>
        <ul v-if="management.uninstall.pending.value?.configurations.length" class="m-0 max-h-40 space-y-1 overflow-y-auto pl-4">
          <li v-for="instance in management.uninstall.pending.value.configurations" :key="instance.id" class="wrap-anywhere">
            {{ instance.name }}
          </li>
        </ul>
        <div class="flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
          <span>同时清理来源与独用下载认证</span>
          <PluginHelpPopover label="卸载清理说明">
            <p class="m-0">
              只清理被卸载版本使用且不再被其他版本引用的下载认证，不删除共享认证、代理或审计记录
            </p>
            <p class="m-0">
              暂时不用可停用配置，无需卸载
            </p>
          </PluginHelpPopover>
        </div>
        <BaseCheckbox v-model="management.uninstall.acknowledged.value" label="确认永久删除，无法恢复" show-label :disabled="management.uninstall.busy.value" />
        <p v-if="management.uninstall.progress.value" role="status" class="m-0 text-cp-xs text-cp-text-secondary">
          {{ management.uninstall.progress.value }}
        </p>
      </div>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="management.showCredentialDelete.value"
      title="删除下载认证"
      description="仍被使用时不可删除，删除后无法恢复"
      destructive
      confirm-text="确认删除"
      :loading="Boolean(management.busyDistributionId.value)"
      @confirm="management.confirmCredentialDelete"
    >
      <p class="m-0 wrap-anywhere text-cp-sm font-normal">
        {{ management.pendingCredential.value?.name }}
      </p>
    </BaseConfirmModal>
  </div>
</template>
