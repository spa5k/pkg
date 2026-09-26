import pathlib,subprocess,os,json,re
out=pathlib.Path('/tmp/pkg-audit/mutations'); work=out/'workspace'; results=json.loads((out/'results.json').read_text())
env=os.environ.copy();env.update({'PATH':'/home/user/.cargo/bin:'+env['PATH'],'CARGO_TARGET_DIR':'/home/user/workspace/target','CARGO_BUILD_JOBS':'1','CARGO_PROFILE_TEST_DEBUG':'0','CARGO_PROFILE_DEV_DEBUG':'0','RUSTUP_TOOLCHAIN':'1.96.1'})
def run(name,tail):
 command=['cargo','test','--locked','-p','pkg-cli']+tail
 r=subprocess.run(command,cwd=work,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=180)
 (out/(name+'.log')).write_text(r.stdout)
 return {'command':command,'exit_code':r.returncode,'test_results':re.findall(r'test result:.*',r.stdout),'log':name+'.log'}
p=work/'crates/pkg-cli/src/commands/execute.rs'; original=p.read_bytes();old='        || value.contains("/nix/")\n';assert original.decode().count(old)==1
try:
 p.write_text(original.decode().replace(old,'',1))
 fake=run('privacy-fake-stays-green',['--test','e2e_fake','--','--exact','every_command_routes_through_typed_fake_core_and_engine_calls_are_exact'])
 keeper=run('privacy-keeper-catches',['--lib','commands::execute::tests::public_result_rejects_private_runtime_material_and_reserved_fields','--','--exact'])
 results.append({'id':'private-result-leak','mutation':'Remove /nix/ rejection from real public result validation','file':'crates/pkg-cli/src/commands/execute.rs','fake_suite':fake,'keeper':keeper,'restored':True})
finally:p.write_bytes(original)
assert p.read_bytes()==original
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
p=work/'crates/pkg-cli/src/commands/doctor.rs'; original=p.read_bytes(); old='            subsystem_check("runtime.managed", &inputs.managed_runtime),\n            subsystem_check("channel.signed", &inputs.channel),\n';assert original.decode().count(old)==1
try:
 p.write_text(original.decode().replace(old,''))
 result=run('doctor-missing-checks',['--test','cli','completion_is_real_static_source_and_doctor_reports_verified_host_state','--','--exact'])
 results.append({'id':'doctor-missing-checks','mutation':'Drop runtime.managed and channel.signed rows from the real doctor report','file':'crates/pkg-cli/src/commands/doctor.rs','test':result,'restored':True})
finally:p.write_bytes(original)
assert p.read_bytes()==original
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
print(json.dumps(results[-2:],indent=2))
