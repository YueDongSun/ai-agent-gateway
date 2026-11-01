import { pathToFileURL } from 'node:url';
import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';
import { install_client_management_navigation, open_client_management } from './client_management_navigation.mjs';

export async function run_contextual_action_checks({ cdp, check }) {
  await cdp.eval_value(`(() => {
    if(window.__aagDemoIsDemo!==true)throw new Error('context action checks require synthetic IPC');
    const element=(id)=>document.querySelector('[data-testid="'+id+'"]');
    const wait_for=async(read)=>{for(let i=0;i<100;i++){const value=read();if(value)return value;await new Promise(r=>setTimeout(r,40));}throw new Error('action UI condition timed out');};
    const click=async(id)=>{const button=await wait_for(()=>{const value=element(id);return value&&!value.disabled?value:null;});button.click();await new Promise(r=>setTimeout(r,70));};
    const set_text=async(id,value)=>{const input=await wait_for(()=>element(id));const prototype=input instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:input instanceof HTMLSelectElement?HTMLSelectElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(prototype,'value').set.call(input,value);input.dispatchEvent(new Event(input instanceof HTMLSelectElement?'change':'input',{bubbles:true}));await new Promise(r=>setTimeout(r,70));};
    window.__aag_action_test={element,wait_for,click,set_text};return true;
  })()`);
  await install_client_management_navigation(cdp);
  await check_in_page(cdp,check,'read-only client and usage views do not inherit floating mutations',async()=>{
    const api=window.__aag_action_test;
    for(const id of ['app-tab-codex','usage_nav']){
      await api.click(id); await api.wait_for(()=>api.element('page-action-save').disabled&&api.element('page-action-delete').disabled);
    }
    return 'ok';
  });
  await check_in_page(cdp,check,'MCP floating save uses existing validation then persists one named definition',async()=>{
    const api=window.__aag_action_test;
    await api.click('mcp_nav'); await api.click('mcp_add');
    await api.set_text('mcp_id','quick_action_fixture');await api.set_text('mcp_name','Quick action fixture');await api.set_text('mcp_config','{"command":"node","args":["fixture.mjs"]}');
    await api.click('page-action-save');
    await api.wait_for(()=>api.element('mcp_server_quick_action_fixture'));
    return {ok:!api.element('mcp_editor')&&api.element('page-action-save').disabled};
  });
  await cdp.send('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:true});
  await check_in_page(cdp,check,'MCP dock stays keyboard-contained and clear of modal controls at390px',async()=>{
    const api=window.__aag_action_test;await api.click('mcp_edit_quick_action_fixture');
    const dialog=await api.wait_for(()=>api.element('mcp_editor'));
    dialog.scrollTop=dialog.scrollHeight;await new Promise(r=>setTimeout(r,100));
    const dock=api.element('page-action-dock');const a=dock.getBoundingClientRect();const b=dialog.getBoundingClientRect();
    return {ok:dialog.contains(dock)&&a.top>=b.bottom&&a.right<=innerWidth&&document.documentElement.scrollWidth<=innerWidth+1,dockTop:a.top,dialogBottom:b.bottom};
  });
  await check_in_page(cdp,check,'MCP floating delete opens and preserves the existing cancellable confirmation',async()=>{
    const api=window.__aag_action_test;await api.click('page-action-delete');
    await api.wait_for(()=>api.element('mcp_delete_dialog'));
    await api.click('mcp_cancel_delete');
    await api.wait_for(()=>!api.element('mcp_delete_dialog'));
    const retained=!!api.element('mcp_editor')&&!!api.element('mcp_server_quick_action_fixture');
    await api.click('page-action-delete');await api.wait_for(()=>api.element('mcp_delete_dialog'));
    await api.click('page-action-delete');await api.wait_for(()=>!api.element('mcp_server_quick_action_fixture'));
    return {ok:retained&&!api.element('mcp_editor')};
  });
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await open_client_management(cdp,'kilo_cli');
  await check_in_page(cdp,check,'client JSON floating save preserves current-document validation and success state',async()=>{
    const api=window.__aag_action_test;await api.click('client-config-edit');
    await api.wait_for(()=>api.element('config-editor-text'));
    const original=api.element('config-editor-text').value;
    await api.set_text('config-editor-text','{invalid');
    await api.wait_for(()=>api.element('page-action-save').disabled&&api.element('config-editor-invalid'));
    await api.set_text('config-editor-text',original+'\n');
    await api.wait_for(()=>!api.element('page-action-save').disabled);
    await api.click('page-action-save');await api.wait_for(()=>!api.element('config-editor-modal'));
    return 'ok';
  });
  await cdp.send('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:true});
  await check_in_page(cdp,check,'DSH floating save opens review and cannot bypass acknowledgement',async()=>{
    const api=window.__aag_action_test;await api.click('dsh_nav');await api.wait_for(()=>api.element('dsh_theme')&&!api.element('dsh_theme').disabled);
    const theme=api.element('dsh_theme').value;await api.set_text('dsh_theme',theme==='dark'?'light':'dark');
    await api.click('page-action-save');const dialog=await api.wait_for(()=>api.element('dsh_preview_dialog'));
    const blocked=api.element('page-action-save').disabled;
    dialog.scrollTop=dialog.scrollHeight;await new Promise(r=>setTimeout(r,80));
    const dock=api.element('page-action-dock').getBoundingClientRect();const bounds=dialog.getBoundingClientRect();
    const clear=dock.top>=bounds.bottom&&dock.right<=innerWidth&&document.documentElement.scrollWidth<=innerWidth+1;
    await api.click('dsh_acknowledge');await api.wait_for(()=>!api.element('page-action-save').disabled);
    await api.click('dsh_cancel_preview');await api.click('dsh_discard_settings');
    return {ok:blocked&&clear,dockTop:dock.top,dialogBottom:bounds.bottom};
  });
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await check_in_page(cdp,check,'leaving an editable DSH context clears its save/delete target on client navigation',async()=>{
    const api=window.__aag_action_test;await api.click('app-tab-codex');
    await api.wait_for(()=>api.element('cli-router-page')&&api.element('page-action-save').disabled&&api.element('page-action-delete').disabled);
    return {ok:!api.element('page-action-dock').textContent.includes('DSH settings')};
  });
  await cdp.eval_value(`document.querySelector('[data-testid="suppliers_nav"]').click()`);
}

async function main(){let runtime;let cdp;let total=0,failed=0;const check=(name,ok,detail='')=>{total++;if(!ok)failed++;process.stdout.write((ok?'PASS':'FAIL')+' '+name+(detail?': '+detail:'')+'\n');};
  try{const browser=find_browser();if(!browser)throw new Error('Supported browser required');runtime=await create_smoke_runtime(browser,{built_demo:process.argv.includes('--built-demo')});cdp=await attach_page(runtime.cdp_origin);await cdp.send('Page.navigate',{url:runtime.demo_url});const deadline=Date.now()+30000;while(Date.now()<deadline&&!await cdp.eval_value('window.__aagDemoIsDemo&&!!document.querySelector("[data-testid=suppliers_page]")'))await sleep(100);await run_contextual_action_checks({cdp,check});check('zero uncaught contextual-action browser exceptions',cdp.exceptions.length===0,cdp.exceptions[0]??'');}
  finally{cdp?.web_socket.close();await runtime?.close();}
  process.stdout.write((total-failed)+'/'+total+' contextual action checks passed\n');if(failed)process.exitCode=1;
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href)main().catch(error=>{process.stderr.write(String(error.stack??error));process.exitCode=1;});
