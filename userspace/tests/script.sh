# The shell test (make user-test): usertest runs `sh /test/script.sh` with
# stdout in a file and compares it with script.expected.
echo hello world
echo "quoted   spaces" 'single $HOME' $HOME
echo -n no newline; echo
mkdir /shell-test
cd /shell-test
pwd
echo content > file.txt
echo more >> file.txt
cat file.txt
cat < file.txt
ls
ls -l /shell-test
false
echo status $?
true ; echo status $?
nosuchcommand
echo not found $?
export GREETING="hi there"
echo "$GREETING" ${GREETING}!
unset GREETING
echo [$GREETING]
mv file.txt renamed.txt
ls
stat renamed.txt > /dev/null
echo stat status $?
rm renamed.txt
ls
cd ..
rm /shell-test
ls /shell-test
echo gone $?
echo done
