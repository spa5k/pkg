import pathlib,subprocess,os,json,re
out=pathlib.Path('/tmp/pkg-audit/mutations');work=out/'workspace';source=pathlib.Path('/home/user/workspace')
for name in ['tools/install/render.py','tests/linux-clean-host/run.sh','crates/pkg-cli/src/commands/execute.rs','crates/pkg-cli/src/commands/doctor.rs']:assert (work/name).read_bytes()==(source/name).read_bytes(),name
env=os.environ.copy();env.update({'PATH':'/home/user/.cargo/bin:'+env['PATH'],'CARGO_TARGET_DIR':'/home/user/workspace/target','CARGO_BUILD_JOBS':'1','CARGO_PROFILE_TEST_DEBUG':'0','CARGO_PROFILE_DEV_DEBUG':'0','RUSTUP_TOOLCHAIN':'1.96.1'})
rows=[]
for name,tail in [('privacy-restored',['--lib','commands::execute::tests::public_result_rejects_private_runtime_material_and_reserved_fields','--','--exact']),('doctor-restored',['--test','cli','completion_is_real_static_source_and_doctor_reports_verified_host_state','--','--exact'])]:
 cmd=['cargo','test','--locked','-p','pkg-cli']+tail;r=subprocess.run(cmd,cwd=work,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=180);(out/(name+'.log')).write_text(r.stdout);rows.append({'name':name,'command':cmd,'exit_code':r.returncode,'test_results':re.findall(r'test result:.*',r.stdout)})
(out/'restored-controls.json').write_text(json.dumps(rows,indent=2)+'\n');print(json.dumps(rows,indent=2))
