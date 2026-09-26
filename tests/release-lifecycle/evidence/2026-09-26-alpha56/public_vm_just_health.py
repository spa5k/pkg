import importlib.util,json
from pathlib import Path
s=importlib.util.spec_from_file_location('lifecycle','/Users/admin/pkg-proof/tests/release-lifecycle/check.py');m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
m.require_disposable('TEST-DISPOSABLE-HOST')
w=Path('/Users/admin/alpha56-public-extra');w.mkdir(mode=0o700)
c=m.Checks(w)
c.package('just','Run public built package')
c.run('Verify public build',[m.CLI,'repair','--verify-only'])
c.run('Public build health with configured PATH',[m.CLI,'doctor'])
(w/'result.json').write_text(json.dumps({'status':'passed','scope':'Public just build use, repair, and doctor with the documented configured PATH','checks':len(c.rows)},indent=2)+'\n')
print('PASS: public just execution, repair, and configured-shell health')
